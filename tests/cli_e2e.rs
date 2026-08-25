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

/// Run trekr with extra environment on top of the isolated database.
fn trekr_env(db: &Path, cwd: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_trekr"));
    command.args(args).current_dir(cwd).env("TREKR_DB", db);
    for (key, value) in vars {
        command.env(key, value);
    }
    command.output().expect("run trekr")
}

#[test]
fn profile_reports_on_stderr_so_stdout_stays_the_answer() {
    let (dir, db) = scratch("profile");
    repo(&dir);

    let out = trekr(&db, &dir, &["--index", "--profile", "--json"]);
    // stdout must still parse as the answer alone.
    let answer = json(&out);
    assert_eq!(answer["indexed"]["files"], 1);

    let timings: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim())
            .expect("the profile is JSON when --json is on");
    let phases: Vec<&str> = timings["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        phases,
        ["scan", "known-diff", "parse", "store-write", "gem-scan"],
        "field names stay stable — a caller graphs these"
    );
    assert_eq!(timings["parsed"], 1);
    assert_eq!(timings["skipped"], 0);
    assert!(timings["jobs"].as_u64().unwrap() >= 1);

    // A second run parses nothing, and the profile says so.
    let again = trekr(&db, &dir, &["--index", "--profile", "--json"]);
    let timings: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&again.stderr).trim()).unwrap();
    assert_eq!(timings["parsed"], 0);
    assert_eq!(timings["skipped"], 1);

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn without_the_flag_no_profile_is_printed() {
    let (dir, db) = scratch("noprofile");
    repo(&dir);
    let out = trekr(&db, &dir, &["--index", "--json"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).trim().is_empty(),
        "profiling must cost nothing you did not ask for"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn jobs_comes_from_the_flag_then_the_environment_then_the_machine() {
    let (dir, db) = scratch("jobs");
    repo(&dir);

    let jobs = |out: &Output| -> u64 {
        serde_json::from_str::<serde_json::Value>(String::from_utf8_lossy(&out.stderr).trim())
            .unwrap()["jobs"]
            .as_u64()
            .unwrap()
    };

    let flagged = trekr(
        &db,
        &dir,
        &["--index", "--profile", "--json", "--jobs", "3"],
    );
    assert_eq!(jobs(&flagged), 3);

    let from_env = trekr_env(
        &db,
        &dir,
        &["--index", "--profile", "--json"],
        &[("TREKR_JOBS", "2")],
    );
    assert_eq!(jobs(&from_env), 2);

    let both = trekr_env(
        &db,
        &dir,
        &["--index", "--profile", "--json", "--jobs", "5"],
        &[("TREKR_JOBS", "2")],
    );
    assert_eq!(jobs(&both), 5, "the flag wins over the environment");

    let auto = trekr_env(
        &db,
        &dir,
        &["--index", "--profile", "--json"],
        &[("TREKR_JOBS", "0")],
    );
    assert!(jobs(&auto) >= 1, "0 means pick for me, never zero workers");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_gem_is_indexed_once_and_a_missing_one_is_reported() {
    let (dir, db) = scratch("gems");
    repo(&dir);

    // A vendored gem, exactly where bundler would put it, plus one the
    // lockfile names and disk does not have.
    let gem = dir.join("vendor/bundle/ruby/3.3.0/gems/widget-0.1.0/lib");
    fs::create_dir_all(&gem).unwrap();
    fs::write(
        gem.join("widget.rb"),
        "module Widget\n  def helpers\n  end\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("Gemfile.lock"),
        // One line per entry: indentation is the whole grammar here, and a
        // `\` continuation is exactly what `cargo fmt` reflows.
        concat!(
            "GEM\n",
            "  remote: https://rubygems.org/\n",
            "  specs:\n",
            "    widget (0.1.0)\n",
            "    absent (9.9.9)\n",
            "\n",
            "DEPENDENCIES\n",
            "  widget\n",
        ),
    )
    .unwrap();

    let first = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(first["gems"]["found"], 1);
    assert_eq!(first["gems"]["indexed"], 1);
    assert_eq!(
        first["gems"]["missing"].as_array().unwrap(),
        &vec!["absent 9.9.9"],
        "a named-but-unlocated gem is a reported hole, not a silent absence"
    );

    // A gem's bytes never change, so a second run reads it again for nothing.
    let again = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(again["gems"]["indexed"], 0);
    assert_eq!(again["gems"]["already_indexed"], 1);

    // And the gem's code answers queries in the project.
    let answer = json(&trekr(&db, &dir, &["--ancestors", "Widget", "--json"]));
    assert_eq!(answer["status"], "resolved");

    let skipped = json(&trekr(&db, &dir, &["--index", "--json", "--no-gems"]));
    assert_eq!(skipped["gems"]["found"], 0, "--no-gems does not look");

    let _ = fs::remove_dir_all(&dir);
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
        &vec!["P", "C", "I", "Base", "Object", "Kernel", "BasicObject"],
        "the core tail is real: Base inherits Object, which is what makes \
         Kernel#puts reachable from C"
    );
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Nope"]).status.code(),
        Some(1)
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Two classes with the same method name, and call sites of each kind.
fn collision_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
    let source = concat!(
        "class Widget\n",           //  1
        "  def save\n",             //  2
        "  end\n",                  //  3
        "end\n",                    //  4
        "class Gadget\n",           //  5
        "  def save\n",             //  6
        "  end\n",                  //  7
        "end\n",                    //  8
        "class Report\n",           //  9
        "  def save(path, mode)\n", // 10
        "  end\n",                  // 11
        "end\n",                    // 12
        "class Job\n",              // 13
        "  def run\n",              // 14
        "    w = Widget.new\n",     // 15
        "    w.save\n",             // 16  confirmed
        "    g = Gadget.new\n",     // 17
        "    g.save\n",             // 18  excluded — resolves to Gadget
        "    thing.save\n",         // 19  possible — untyped
        "    other.save(1, 2)\n",   // 20  excluded — arity
        "  end\n",                  // 21
        "end\n",                    // 22
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
fn refs_narrowed_by_receiver_separates_confirmed_from_possible() {
    let (dir, db) = scratch("refsnarrow");
    collision_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let answer = json(&trekr(&db, &dir, &["--refs", "Widget#save", "--json"]));
    assert_eq!(answer["owner"], "Widget");
    assert_eq!(answer["definition"][0]["line"], 2);

    let rows = answer["references"].as_array().unwrap();
    let tiers: Vec<(u64, &str)> = rows
        .iter()
        .map(|r| (r["line"].as_u64().unwrap(), r["tier"].as_str().unwrap()))
        .collect();
    assert_eq!(
        tiers,
        [(16, "confirmed"), (19, "possible")],
        "the typed Gadget call and the wrong-arity call are gone from the list"
    );
    assert_eq!(rows[0]["receiver_type"], "Widget");
    assert_eq!(rows[0]["owner"], "Widget");

    // The count is the product: it is what a grep cannot produce.
    assert_eq!(answer["counts"]["confirmed"], 1);
    assert_eq!(answer["counts"]["possible"], 1);
    assert_eq!(
        answer["counts"]["excluded"], 2,
        "one ruled out by receiver, one by arity"
    );
    // The three reasons differ in strength, so they are counted apart: only
    // `different_owner` is positive evidence.
    assert_eq!(answer["counts"]["excluded_different_owner"], 1);
    assert_eq!(answer["counts"]["excluded_arity"], 1);
    assert_eq!(answer["counts"]["excluded_no_such_method"], 0);

    // And the claim is auditable rather than merely asserted.
    let audited = json(&trekr(
        &db,
        &dir,
        &["--refs", "Widget#save", "--json", "--include-excluded"],
    ));
    let rulings: Vec<&str> = audited["references"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["ruling"].as_str())
        .collect();
    assert_eq!(
        rulings.len(),
        2,
        "every exclusion names its reason: {rulings:?}"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refs_for_a_class_method_are_a_different_question() {
    let (dir, db) = scratch("refssingleton");
    git(&dir, &["init", "-q"]);
    fs::write(
        dir.join("app.rb"),
        concat!(
            "class Widget\n",        // 1
            "  def self.save\n",     // 2
            "  end\n",               // 3
            "  def save\n",          // 4
            "  end\n",               // 5
            "end\n",                 // 6
            "class Job\n",           // 7
            "  def run\n",           // 8
            "    Widget.save\n",     // 9  the class method
            "    Widget.new.save\n", // 10 the instance method
            "  end\n",               // 11
            "end\n",                 // 12
        ),
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

    let class_method = json(&trekr(&db, &dir, &["--refs", "Widget.save", "--json"]));
    assert_eq!(class_method["definition"][0]["line"], 2);
    assert_eq!(class_method["counts"]["confirmed"], 1);
    let rows = class_method["references"].as_array().unwrap();
    assert_eq!(rows[0]["line"], 9);

    let instance_method = json(&trekr(&db, &dir, &["--refs", "Widget#save", "--json"]));
    assert_eq!(instance_method["definition"][0]["line"], 4);
    assert_eq!(
        instance_method["counts"]["excluded"], 1,
        "the class-method call is excluded from the instance method's references"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_bare_name_still_reports_every_mention_and_now_says_what_each_resolves_to() {
    let (dir, db) = scratch("refsbare");
    collision_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let rows = json(&trekr(&db, &dir, &["--refs", "save", "--json"]));
    let rows = rows.as_array().unwrap();
    assert!(
        rows.iter().any(|r| r["role"] == "definition"),
        "a bare name narrows nothing, so definitions stay in the answer"
    );
    let confirmed: Vec<&str> = rows
        .iter()
        .filter(|r| r["tier"] == "confirmed")
        .map(|r| r["owner"].as_str().unwrap())
        .collect();
    assert_eq!(
        confirmed,
        ["Widget", "Gadget"],
        "each typed call site says which owner it reaches"
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn refs_on_an_unknown_owner_is_a_definitive_no() {
    let (dir, db) = scratch("refsunknown");
    collision_repo(&dir);
    trekr(&db, &dir, &["--index"]);
    assert_eq!(
        trekr(&db, &dir, &["--refs", "Nope#save"]).status.code(),
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
    // An outline is parsed, not looked up, so it answers before any index
    // exists. Exit 1 is reserved for a file that really defines nothing.
    assert_eq!(
        trekr(&db, &dir, &["--symbols", "widget.rb"]).status.code(),
        Some(0)
    );
    fs::write(dir.join("blank.rb"), "# just a comment\n").unwrap();
    assert_eq!(
        trekr(&db, &dir, &["--symbols", "blank.rb"]).status.code(),
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

/// The unit is the file's own checkout, not the process's directory.
///
/// An agent asks about a position from wherever it is standing, which is
/// routinely a different repo — and the answer used to depend on that, silently
/// and wrongly: the tree was built for the cwd, so a query about another repo's
/// file resolved against a namespace that had never heard of it.
#[test]
fn a_position_resolves_against_its_own_repo_not_the_current_directory() {
    let (dir, db) = scratch("elsewhere");
    repo(&dir);
    // A second repo, indexed and never visited.
    let (other, _) = scratch("elsewhere-other");
    git(&other, &["init", "-q"]);
    fs::write(
        other.join("app.rb"),
        "module Widgets\n  class Gauge\n  end\n  Gauge\nend\n",
    )
    .unwrap();
    git(&other, &["add", "-A"]);
    git(
        &other,
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
    trekr(&db, &other, &["--index"]);

    let target = format!("{}:4:3", other.join("app.rb").display());
    let from_elsewhere = json(&trekr(&db, &dir, &["--def", &target, "--json"]));
    assert_eq!(
        from_elsewhere["status"], "resolved",
        "standing in another repo entirely: {from_elsewhere}"
    );
    assert_eq!(from_elsewhere["fqn"], "Widgets::Gauge");

    // And the same answer from inside, which is what used to be the only way
    // to get one.
    let from_inside = json(&trekr(&db, &other, &["--def", &target, "--json"]));
    assert_eq!(from_inside["fqn"], from_elsewhere["fqn"]);

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
}

/// Two trekrs on one machine must not take turns wiping each other's index.
///
/// A version mismatch drops and reindexes (DEC-009), which is right when the
/// binary is *newer* than the database. The other direction — an older binary
/// meeting a newer database — is a stale install about to destroy work, and it
/// looked exactly like "trekr has never been run here".
#[test]
fn an_older_binary_refuses_a_newer_database_rather_than_dropping_it() {
    let (dir, db) = scratch("newerdb");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    // Forge a database from the future.
    let out = Command::new("sqlite3")
        .arg(&db)
        .arg("PRAGMA user_version = 9999;")
        .output()
        .expect("sqlite3 available");
    assert!(out.status.success());

    let refused = trekr(&db, &dir, &["--status"]);
    assert_eq!(
        refused.status.code(),
        Some(2),
        "a request that cannot be served, not an empty answer"
    );
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("upgrade trekr"),
        "and it says what to do: {message}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `--usage` turns the serve log into the dogfood signal it was written for:
/// which operations agents call, and how often the answer was empty.
#[test]
fn usage_summarizes_the_serve_log_and_says_nothing_when_it_is_empty() {
    let (dir, db) = scratch("usage");
    fs::create_dir_all(&dir).unwrap();
    let log = dir.join("serve.log");
    fs::write(&log, "").unwrap();

    // Nothing logged is a definitive "no", not a failure.
    let empty = trekr_env(
        &db,
        &dir,
        &["--usage"],
        &[("TREKR_LOG", log.to_str().unwrap())],
    );
    assert_eq!(empty.status.code(), Some(1));

    fs::write(
        &log,
        concat!(
            r#"{"ts":"2026-01-01T00:00:00.000Z","event":"start"}"#,
            "\n",
            r#"{"ts":"2026-01-01T00:00:01.000Z","event":"request","op":"textDocument/definition","ms":4.0,"answered":2,"status":"ok"}"#,
            "\n",
            r#"{"ts":"2026-01-01T00:00:02.000Z","event":"request","op":"textDocument/definition","ms":2.0,"answered":0,"status":"ok"}"#,
            "\n",
            r#"{"ts":"2026-01-01T00:00:03.000Z","event":"request","op":"textDocument/hover","ms":1.0,"answered":1,"status":"ok"}"#,
            "\n",
            // A line the log did not write cleanly must not stop the report.
            "{not json\n",
        ),
    )
    .unwrap();

    let out = trekr_env(
        &db,
        &dir,
        &["--usage", "--json"],
        &[("TREKR_LOG", log.to_str().unwrap())],
    );
    assert_eq!(out.status.code(), Some(0));
    let rows = json(&out);
    let rows = rows.as_array().expect("a row per operation");
    // Most-used first: the ranking is the point of the report.
    assert_eq!(rows[0]["op"], "textDocument/definition");
    assert_eq!(rows[0]["calls"], 2);
    assert_eq!(rows[0]["answered"], 1);
    assert_eq!(rows[0]["empty"], 1, "an empty answer is counted as one");
    assert_eq!(rows[1]["op"], "textDocument/hover");

    let _ = fs::remove_dir_all(&dir);
}

/// `--explain` renders the disclosure `--json` already carries. CLAUDE.md and
/// PLAN promised the flag from the start; only the rendering was missing.
#[test]
fn explain_renders_the_disclosure_the_json_already_carries() {
    let (dir, db) = scratch("explain");
    git(&dir, &["init", "-q"]);
    // Two classes define `ship`, so the answer is ambiguous with a candidate
    // list — the case where an explanation is worth reading.
    fs::write(
        dir.join("app.rb"),
        "class Widget\n  def ship\n  end\nend\nclass Other\n  def ship\n  end\nend\n\
         class Job\n  def run\n    @widget.ship\n  end\n\
         \x20 def sweep\n    thing.ship\n  end\nend\n",
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

    let plain = stdout(&trekr(&db, &dir, &["--def", "app.rb:11:13"]));
    assert!(!plain.contains("status"), "quiet without the flag: {plain}");

    let explained = stdout(&trekr(&db, &dir, &["--def", "app.rb:11:13", "--explain"]));
    for expected in ["status", "ambiguous", "receiver_name", "agreement"] {
        assert!(
            explained.contains(expected),
            "{expected} missing: {explained}"
        );
    }

    // A residue is where the ranked candidates live, and why they ranked is
    // the part worth reading.
    let residue = stdout(&trekr(&db, &dir, &["--def", "app.rb:14:11", "--explain"]));
    assert!(residue.contains("candidates"), "{residue}");
    assert!(residue.contains("1. "), "numbered by rank: {residue}");
    // Every line restates a field of the answer, so the two surfaces cannot
    // drift: whatever --explain claims, --json must also say.
    let structured = json(&trekr(&db, &dir, &["--def", "app.rb:11:13", "--json"]));
    assert_eq!(structured["status"], "ambiguous");
    assert_eq!(structured["resolved_via"], "receiver_name");
    let residue_json = json(&trekr(&db, &dir, &["--def", "app.rb:14:11", "--json"]));
    assert!(!residue_json["candidates"].as_array().unwrap().is_empty());

    let _ = fs::remove_dir_all(&dir);
}

/// A gem on its own is a tree of one gem plus core, so a method it gets from a
/// sibling gem is unreachable by construction (DEC-029). The fix answers from
/// an app that resolves the gem — which needs two checkouts, and so lives here
/// rather than in the testbed.
#[test]
fn a_gem_position_answers_from_an_app_that_resolves_it() {
    let (app, db) = scratch("gemctx-app");
    let (gems, _) = scratch("gemctx-gems");

    // Two gems: one defines a method, the other calls it. Laid out the way
    // bundler does, because that is how they are located.
    // `$GEM_HOME/gems/<name>-<version>/lib` is the layout gems are located by.
    let helper = gems.join("gems/helper-1.0.0/lib");
    let user = gems.join("gems/user-1.0.0/lib");
    fs::create_dir_all(&helper).unwrap();
    fs::create_dir_all(&user).unwrap();
    fs::write(
        helper.join("helper.rb"),
        "class Module\n  def helper_macro(name)\n  end\nend\n",
    )
    .unwrap();
    fs::write(
        user.join("user.rb"),
        "class Consumer\n  helper_macro :thing\nend\n",
    )
    .unwrap();

    // An app whose lockfile resolves both.
    git(&app, &["init", "-q"]);
    fs::write(app.join("app.rb"), "class Widget\nend\n").unwrap();
    fs::write(
        app.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    helper (1.0.0)\n    user (1.0.0)\n\
         \nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  helper\n  user\n",
    )
    .unwrap();
    git(&app, &["add", "-A"]);
    git(
        &app,
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

    let out = trekr_env(
        &db,
        &app,
        &["--index"],
        &[("GEM_HOME", gems.to_str().unwrap())],
    );
    assert!(out.status.success(), "indexed the app and its gems");

    // The call lives in the `user` gem; the definition lives in `helper`.
    let spec = format!("{}:2:3", user.join("user.rb").display());
    let answer = json(&trekr(&db, &app, &["--def", &spec, "--json"]));
    assert_eq!(
        answer["status"], "resolved",
        "the sibling gem's method is reachable: {answer}"
    );
    assert!(
        answer["sites"][0]["path"]
            .as_str()
            .unwrap()
            .contains("helper-1.0.0"),
        "and it points at the gem that defines it: {answer}"
    );
    // An answer that depends on which app supplied the ancestors says which.
    assert_eq!(
        answer["context"].as_str(),
        app.canonicalize().unwrap().to_str(),
        "the answering context is disclosed"
    );

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
}
