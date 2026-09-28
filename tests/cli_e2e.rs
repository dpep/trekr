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
    let _ = fs::remove_dir_all(db.with_extension("trees"));
    // The usage counts beside the store outlive a run, and a later run that
    // reuses the process id would read them as its own.
    let _ = fs::remove_file(db.with_extension("usage.db"));
    fs::create_dir_all(&dir).unwrap();
    (dir, db)
}

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
    neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(args)
        .current_dir(cwd)
        .env("TREKR_DB", db)
        .output()
        .expect("run trekr")
}

/// A command whose caller is nobody in particular. Whoever runs the suite — an
/// agent, CI — sets variables `--usage` reads to name the caller, and the
/// usage tests assert on that name.
fn neutral(mut command: Command) -> Command {
    for var in [
        "CLAUDECODE",
        "CLAUDE_CODE_ENTRYPOINT",
        "AI_AGENT",
        "CURSOR_TRACE_ID",
        "CURSOR_AGENT",
        "CI",
        "GITHUB_ACTIONS",
        "TREKR_USAGE",
    ] {
        command.env_remove(var);
    }
    command
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

    // `#` is Ruby's notation for an instance method, and only for one.
    let text = stdout(&trekr(&db, &dir, &["--symbols", "widget.rb"]));
    assert!(text.contains("class    Widget"), "{text}");
    assert!(text.contains("#resize"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

/// Every `path` in an answer, collected with the `root` beside it.
fn paths_in(value: &serde_json::Value, found: &mut Vec<(String, serde_json::Value)>) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(path)) = map.get("path") {
                found.push((path.clone(), map.get("root").cloned().unwrap_or_default()));
            }
            map.values().for_each(|v| paths_in(v, found));
        }
        serde_json::Value::Array(items) => items.iter().for_each(|v| paths_in(v, found)),
        _ => {}
    }
}

#[test]
fn every_path_is_relative_to_the_root_beside_it() {
    let (dir, db) = scratch("paths");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);
    let root = fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();

    let answers = [
        json(&trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"])),
        json(&trekr(&db, &dir, &["--refs", "Widget#helper", "--json"])),
        json(&trekr(&db, &dir, &["--refs", "helper", "--json"])),
        json(&trekr(&db, &dir, &["Widget#helper", "--json"])),
        json(&trekr(
            &db,
            &dir,
            &["--dead", &format!("{root}/widget.rb"), "--json"],
        )),
        json(&trekr(&db, &dir, &["--symbols", "widget.rb", "--json"])),
    ];
    for answer in &answers {
        let mut found = Vec::new();
        paths_in(answer, &mut found);
        assert!(!found.is_empty(), "{answer}");
        for (path, at) in found {
            assert_eq!(path, "widget.rb", "{answer}");
            assert_eq!(at, serde_json::json!(root), "{answer}");
        }
    }

    // `query` is what was typed, wherever the caller stands.
    let absolute = format!("{root}/widget.rb:7:5");
    for spec in ["widget.rb:7:5", absolute.as_str()] {
        let answer = json(&trekr(&db, &dir, &["--def", spec, "--json"]));
        assert_eq!(answer["query"], spec, "{answer}");
    }

    // Text writes a path in the checkout relative to it, definitions included.
    let text = stdout(&trekr(&db, &dir, &["--refs", "Widget#helper"]));
    assert!(text.starts_with("widget.rb:12:7  definition"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn indexes_edits_and_untracked_files_without_touching_the_git_index() {
    let (dir, db) = scratch("worktree");
    repo(&dir);
    fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
    fs::create_dir_all(dir.join("lib/deep")).unwrap();
    fs::create_dir_all(dir.join("ignored")).unwrap();
    fs::write(dir.join("lib/deep/fresh.rb"), "class Fresh\nend\n").unwrap();
    fs::write(dir.join("ignored/skip.rb"), "class Skip\nend\n").unwrap();
    fs::write(
        dir.join("widget.rb"),
        "class Widget\n  def edited\n  end\nend\n",
    )
    .unwrap();
    let git_index = || {
        fs::metadata(dir.join(".git/index"))
            .unwrap()
            .modified()
            .unwrap()
    };
    let before = git_index();

    let indexed = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(indexed["indexed"]["files"], 2, "{indexed}");
    assert_eq!(indexed["indexed"]["parsed"], 2, "the edit and the new file");
    assert_eq!(
        git_index(),
        before,
        "the freshness probe watches .git/index, so a scan must not rewrite it"
    );
    let found = stdout(&trekr(&db, &dir, &["--refs", "edited"]));
    assert!(found.contains("widget.rb"), "the edit, not HEAD: {found}");

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
    let mut command = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")));
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
        [
            "scan",
            "known-diff",
            "parse",
            "store-write",
            "gem-scan",
            "analyze"
        ],
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
    let phases: Vec<&str> = timings["phases"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert!(
        !phases.contains(&"known-diff"),
        "an unchanged map never loads the known blobs: {phases:?}"
    );

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

/// A name is asked about in the checkout `--context` names, from anywhere,
/// as a position already could be.
#[test]
fn context_points_a_name_query_at_a_checkout() {
    let (dir, db) = scratch("context");
    repo(&dir);
    trekr(&db, &dir, &["--index", "--no-gems"]);
    let elsewhere = std::env::temp_dir();
    let context = dir.to_string_lossy().into_owned();

    let card = json(&trekr(
        &db,
        &elsewhere,
        &["Widget#resize", "--context", &context, "--json"],
    ));
    assert_eq!(card["status"], "resolved");
    let refs = json(&trekr(
        &db,
        &elsewhere,
        &["--refs", "Widget#helper", "--context", &context, "--json"],
    ));
    assert_eq!(refs["counts"]["confirmed"], 1);
    let chain = json(&trekr(
        &db,
        &elsewhere,
        &["--ancestors", "Widget", "--context", &context, "--json"],
    ));
    assert_eq!(chain["ancestors"][0], "Widget");

    let outline = trekr(
        &db,
        &dir,
        &["--symbols", "widget.rb", "--context", &context],
    );
    assert_eq!(outline.status.code(), Some(64), "an outline names its file");
    let missing = trekr(&db, &elsewhere, &["Widget", "--context", "/no/such/dir"]);
    assert_eq!(missing.status.code(), Some(66));

    let _ = fs::remove_dir_all(&dir);
}

/// Most gems commit no lockfile; what their gemspec declares is resolved to
/// what is installed instead (DEC-134), and the answer says which it was.
#[test]
fn with_no_lockfile_the_gemspecs_dependencies_are_resolved() {
    let (dir, db) = scratch("declared");
    repo(&dir);
    for version in ["0.1.0", "0.2.0", "1.0.0"] {
        let lib = dir.join(format!(
            "vendor/bundle/ruby/3.3.0/gems/widget-{version}/lib"
        ));
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("widget.rb"), "module Widget\nend\n").unwrap();
    }
    fs::write(
        dir.join("app.gemspec"),
        "Gem::Specification.new do |s|\n  s.add_development_dependency \"widget\", \"< 1\"\n  s.add_dependency \"absent\"\nend\n",
    )
    .unwrap();

    let answer = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(answer["gems"]["lockfile"], false);
    assert_eq!(answer["gems"]["resolved_from"], "declared");
    assert_eq!(answer["gems"]["found"], 1, "the highest below 1: 0.2.0");
    assert_eq!(answer["gems"]["missing"], serde_json::json!(["absent *"]));
    let text = stdout(&trekr(&db, &dir, &["--index"]));
    assert!(text.contains("highest installed version"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_gem_is_indexed_once_and_a_missing_one_is_reported() {
    let (dir, db) = scratch("gems");
    repo(&dir);

    // No lockfile, no gems: said, not left to look like an empty bundle.
    let bare = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(bare["gems"]["lockfile"], false);
    assert!(bare["gems"].get("resolved_from").is_none());
    assert!(stdout(&trekr(&db, &dir, &["--index"])).contains("no Gemfile.lock"));
    let quiet = stdout(&trekr(&db, &dir, &["--index", "--no-gems"]));
    assert!(
        !quiet.contains("Gemfile"),
        "--no-gems asked for none: {quiet}"
    );

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
    assert_eq!(first["gems"]["lockfile"], true);
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

/// Two gems can ship a byte-identical file. It is one blob: parsed once, and
/// the second gem's map points at the facts the first one wrote — rewriting
/// the blob would orphan the first gem's rows.
#[test]
fn a_file_two_gems_share_is_parsed_once_and_answers_for_both() {
    let (dir, db) = scratch("gems-shared");
    repo(&dir);
    let shared = "module Shared\n  def helpers\n  end\nend\n";
    for (gem, own) in [("alpha-1.0.0", "Alpha"), ("beta-1.0.0", "Beta")] {
        let lib = dir.join(format!("vendor/bundle/ruby/3.3.0/gems/{gem}/lib"));
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("shared.rb"), shared).unwrap();
        fs::write(
            lib.join("own.rb"),
            format!("class {own}\n  include Shared\nend\n"),
        )
        .unwrap();
    }
    fs::write(
        dir.join("Gemfile.lock"),
        concat!(
            "GEM\n",
            "  remote: https://rubygems.org/\n",
            "  specs:\n",
            "    alpha (1.0.0)\n",
            "    beta (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  alpha\n",
            "  beta\n",
        ),
    )
    .unwrap();

    let out = trekr(&db, &dir, &["--index", "--profile", "--json"]);
    assert_eq!(json(&out)["gems"]["indexed"], 2);
    let timings: serde_json::Value =
        serde_json::from_str(String::from_utf8_lossy(&out.stderr).trim()).unwrap();
    // The app's file, both gems' own files, and the shared one once.
    assert_eq!(timings["parsed"], 4);

    for class in ["Alpha", "Beta"] {
        let answer = json(&trekr(&db, &dir, &["--ancestors", class, "--json"]));
        let chain: Vec<&str> = answer["ancestors"]
            .as_array()
            .unwrap_or_else(|| panic!("{answer}"))
            .iter()
            .map(|n| n.as_str().unwrap())
            .collect();
        assert!(chain.contains(&"Shared"), "{class}: {chain:?}");
    }

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
        vec!["--gc", "--dry-run", "--ndjson"],
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
        .map(|r| (r["role"].as_str().unwrap(), r["receiver"].as_str()))
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
    assert_eq!(answer["definition"][0]["line"], 3);
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
    assert_eq!(answer["definition"][0]["line"], 4);
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

#[test]
fn a_constant_card_on_a_split_name_lists_each_declaration() {
    let (dir, db) = scratch("splitcard");
    git(&dir, &["init", "-q"]);
    fs::create_dir_all(dir.join("fake")).unwrap();
    fs::create_dir_all(dir.join("models")).unwrap();
    fs::write(dir.join("fake/post.rb"), "Post = Struct.new(:title)\n").unwrap();
    fs::write(
        dir.join("models/post.rb"),
        "class Record\nend\nclass Post < Record\nend\n",
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

    let card = json(&trekr(&db, &dir, &["Post", "--json"]));
    assert_eq!(card["status"], "ambiguous", "{card}");
    // A Struct has no superclass line, so "two superclasses" would be wrong.
    let text = stdout(&trekr(&db, &dir, &["Post"]));
    assert!(text.starts_with("Post is 2 different classes"), "{text}");
    assert_eq!(card["variants"].as_array().map(Vec::len), Some(2), "{card}");
    assert_eq!(
        card["unresolved_ancestors"],
        serde_json::json!([]),
        "both superclasses resolve, each in its own declaration's chain"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A Ruby core definition lands on a file that exists, for the command line
/// as for the editor: `<core>/String.rb` named nothing a caller could open.
#[test]
fn a_core_definition_is_a_file_that_exists() {
    let (dir, db) = scratch("core-file");
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    trekr(&db, &dir, &["--index"]);
    let answer = json(&trekr(&db, &dir, &["--def", "use.rb:1:5", "--json"]));
    let site = &answer["definition"][0];
    assert_eq!(site["path"], "String.rb", "{answer}");
    let root = site["root"].as_str().expect("a root, not null");
    let stub = fs::read_to_string(Path::new(root).join("String.rb")).unwrap();
    assert!(stub.contains("def upcase"), "{stub}");
    let text = stdout(&trekr(&db, &dir, &["--def", "use.rb:1:5"]));
    assert!(text.contains("core/String.rb:"), "{text}");
    let _ = fs::remove_dir_all(&dir);
}

/// One fact, one field name, in every command that reports it (DEC-080).
#[test]
fn the_same_fact_has_the_same_name_in_every_command() {
    let (dir, db) = scratch("names");
    repo(&dir);
    fs::write(
        dir.join("use.rb"),
        "w = Widget.new\nw.resize(1)\nx.resize(2)\n",
    )
    .unwrap();
    trekr(&db, &dir, &["--index"]);
    let keys = |v: &serde_json::Value| -> Vec<String> {
        let mut keys: Vec<String> = v.as_object().unwrap().keys().cloned().collect();
        keys.sort();
        keys
    };

    let chain = json(&trekr(&db, &dir, &["--ancestors", "Widget", "--json"]));
    let card = json(&trekr(&db, &dir, &["Widget", "--json"]));
    for answer in [&chain, &card] {
        assert_eq!(answer["query"], "Widget", "{answer}");
        assert_eq!(answer["fqn"], "Widget", "{answer}");
        assert_eq!(
            answer["unresolved_ancestors"],
            serde_json::json!(["Base", "Trackable"]),
            "{answer}"
        );
    }
    assert!(!keys(&chain).contains(&"unresolved".to_string()), "{chain}");

    // A receiver is `receiver`/`receiver_text`, by name or narrowed.
    let rows = json(&trekr(&db, &dir, &["--refs", "resize", "--json"]));
    let call = rows
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["line"] == 2)
        .unwrap();
    assert_eq!(
        (&call["receiver"], &call["receiver_text"]),
        (&"local".into(), &"w".into())
    );
    let narrowed = json(&trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]));
    assert_eq!(narrowed["references"][0]["receiver"], "local", "{narrowed}");

    // Where a thing is defined is `definition`, present even when unknown.
    let resolved = json(&trekr(&db, &dir, &["--def", "use.rb:2:3", "--json"]));
    assert_eq!(resolved["definition"][0]["line"], 6, "{resolved}");
    let residue = json(&trekr(&db, &dir, &["--def", "use.rb:3:3", "--json"]));
    assert_eq!(residue["definition"], serde_json::json!([]), "{residue}");
    assert!(
        residue["candidates"][0]["site"]["line"].is_number(),
        "{residue}"
    );

    // Every located row has a line and a column.
    let dead = json(&trekr(&db, &dir, &["--dead", "widget.rb", "--json"]));
    for row in dead["candidates"].as_array().unwrap() {
        assert!(row["col"].is_number(), "{row}");
    }

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
            "class Tool\n",          // 13
            "  def fix\n",           // 14
            "  end\n",               // 15
            "end\n",                 // 16
            "Tool.new.fix\n",        // 17
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

    // The tally is printed whether or not anything was ruled out.
    let text = stdout(&trekr(&db, &dir, &["--refs", "Tool#fix"]));
    assert!(
        text.contains("1 confirmed, 0 possible, 0 excluded of 1 same-name call sites"),
        "{text}"
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
    let text = stdout(&trekr(&db, &dir, &["--refs", "Widget"]));
    assert!(
        text.lines().all(|line| line == line.trim_end()),
        "no line ends in padding: {text:?}"
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

/// An owner that resolves is not an answer about the method. When nothing in
/// its ancestors defines the name, the status says so, in both modes and both
/// commands — and only when the whole chain was seen.
#[test]
fn a_method_the_owner_does_not_have_is_named_as_such() {
    let (dir, db) = scratch("nosuchmethod");
    collision_repo(&dir);
    fs::write(dir.join("thing.rb"), "class Thing < Unindexed::Base\nend\n").unwrap();
    fs::write(
        dir.join("proxy.rb"),
        "class Catcher\n  def method_missing(name, *args)\n  end\nend\nclass Proxy < Catcher\nend\n",
    )
    .unwrap();
    trekr(&db, &dir, &["--index"]);

    let defined = json(&trekr(&db, &dir, &["--refs", "Widget#save", "--json"]));
    assert_eq!(defined["status"], "resolved");

    for args in [&["--refs", "Widget#nope"][..], &["Widget#nope"]] {
        let out = trekr(&db, &dir, &[args, &["--json"]].concat());
        assert_eq!(out.status.code(), Some(1), "{args:?}");
        let answer = json(&out);
        assert_eq!(answer["status"], "no_such_method", "{args:?}: {answer}");
        let reason = answer["reason"].as_str().unwrap();
        assert!(
            reason.contains("Widget") && reason.contains("nope"),
            "{args:?}: {reason}"
        );
        let text = stdout(&trekr(&db, &dir, args));
        assert!(
            text.contains(reason),
            "{args:?}: the text says the same: {text}"
        );
    }

    // A chain with a hole in it cannot rule the method out.
    let answer = json(&trekr(&db, &dir, &["--refs", "Thing#nope", "--json"]));
    assert_eq!(answer["status"], "residue", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Unindexed::Base")),
        "{answer}"
    );

    // Nor can a `method_missing` in it, which answers any name.
    let answer = json(&trekr(&db, &dir, &["Proxy#nope", "--json"]));
    assert_eq!(answer["status"], "residue", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Catcher defines method_missing")),
        "{answer}"
    );

    // And an owner that does not resolve says that, rather than nothing.
    let answer = json(&trekr(&db, &dir, &["--refs", "Nope#save", "--json"]));
    assert_eq!(answer["status"], "residue", "{answer}");
    assert!(
        answer["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Nope"))
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A method that provably does not exist, or an owner that does not, has no
/// references: the same-name call sites belong to other owners. Text and JSON
/// agree on that, in the exit code and in what they list.
#[test]
fn refs_to_a_method_that_does_not_exist_list_nothing_in_either_mode() {
    let (dir, db) = scratch("refsnothing");
    collision_repo(&dir);
    trekr(&db, &dir, &["--index"]);

    for query in ["Job#save", "Nope#save"] {
        let text = trekr(&db, &dir, &["--refs", query]);
        assert_eq!(text.status.code(), Some(1), "{query}: {}", stdout(&text));
        assert!(
            !stdout(&text).contains("app.rb:"),
            "{query}: no site is listed: {}",
            stdout(&text)
        );
        let out = trekr(&db, &dir, &["--refs", query, "--json"]);
        assert_eq!(out.status.code(), Some(1), "{query}");
        let answer = json(&out);
        assert_eq!(
            answer["references"],
            serde_json::json!([]),
            "{query}: {answer}"
        );
        assert!(
            answer["hint"]
                .as_str()
                .is_some_and(|hint| hint.contains("--refs save")),
            "{query}: points at the bare name: {answer}"
        );
    }

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

/// Every error exits on its own sysexits code — never 1 (an answer: nothing)
/// or 2 (no answer yet) — and a structured caller gets it as one object on
/// stdout, the message on stderr either way (DEC-067).
#[test]
fn an_error_exits_on_its_own_code_and_speaks_json_when_asked() {
    let (dir, db) = scratch("errors");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    // Outside any checkout, deliberately not `git init`ed.
    let (plain, _) = scratch("errors-plain");
    let no_git = dir.join("empty-path");
    fs::create_dir_all(&no_git).unwrap();
    let no_git = no_git.to_string_lossy().into_owned();

    // (where, args, extra env, exit, kind, what stderr names)
    type Case<'a> = (
        &'a Path,
        &'a [&'a str],
        &'a [(&'a str, &'a str)],
        i32,
        &'a str,
        &'a str,
    );
    let cases: &[Case] = &[
        (
            &dir,
            &["--no-such-flag"],
            &[],
            64,
            "usage",
            "--no-such-flag",
        ),
        (&dir, &["--gc", "--older-than", "7"], &[], 64, "usage", "7"),
        (
            &dir,
            &["--def", "widget.rb"],
            &[],
            64,
            "usage",
            "FILE:LINE:COL",
        ),
        (&dir, &["not a thing"], &[], 64, "usage", "Expected"),
        (
            &dir,
            &["Foo::Bar#baz#qux"],
            &[],
            64,
            "usage",
            "not a method",
        ),
        (
            &dir,
            &["--refs", "widget#resize"],
            &[],
            64,
            "usage",
            "constant",
        ),
        (&dir, &[], &[], 64, "usage", "nothing to do"),
        (
            &dir,
            &["--def", "widget.rb:0:0"],
            &[],
            64,
            "usage",
            "count from 1",
        ),
        (&dir, &["widget.rb:3:0"], &[], 64, "usage", "count from 1"),
        (&dir, &["--def", ".:1:1"], &[], 64, "usage", "directory"),
        (&dir, &["--symbols", "."], &[], 64, "usage", "directory"),
        (
            &dir,
            &["--def", "gone.rb:1:1"],
            &[],
            66,
            "not_found",
            "gone.rb",
        ),
        (&dir, &["--dead", "gone"], &[], 66, "not_found", "gone"),
        (
            &dir,
            &["--symbols", "gone.rb"],
            &[],
            66,
            "not_found",
            "gone.rb",
        ),
        (
            &dir,
            &["--index", "gone/deeper"],
            &[],
            66,
            "not_found",
            "gone/deeper",
        ),
        (
            &plain,
            &["--index"],
            &[],
            66,
            "not_a_repo",
            "not a git repository",
        ),
        (
            &plain,
            &["--refs", "Widget#resize"],
            &[],
            66,
            "not_a_repo",
            "not a git repository",
        ),
        (&dir, &["--index"], &[("PATH", &no_git)], 69, "git", "git"),
    ];
    for (cwd, args, env, code, kind, names) in cases {
        let text = trekr_env(&db, cwd, args, env);
        assert_eq!(text.status.code(), Some(*code), "{args:?}: {text:?}");
        assert_eq!(
            stdout(&text),
            "",
            "{args:?}: text mode keeps stdout for answers"
        );
        let stderr = String::from_utf8_lossy(&text.stderr);
        assert!(stderr.contains(names), "{args:?} names {names}: {stderr}");

        for (flag, compact) in [("--json", false), ("--ndjson", true), ("-j", false)] {
            let args: Vec<&str> = args.iter().copied().chain([flag]).collect();
            let out = trekr_env(&db, cwd, &args, env);
            assert_eq!(out.status.code(), Some(*code), "{args:?}");
            assert_eq!(
                out.stdout.iter().filter(|b| **b == b'\n').count() == 1,
                compact
            );
            let error = json(&out);
            let mut keys: Vec<&String> = error.as_object().unwrap().keys().collect();
            keys.sort();
            assert_eq!(keys, ["code", "error", "kind"], "{args:?}: {error}");
            assert_eq!(error["kind"], *kind, "{args:?}: {error}");
            assert_eq!(error["code"], *code, "the object and the process agree");
            let message = error["error"].as_str().unwrap();
            assert!(message.contains(names), "{args:?}: {message}");
            // The prefix tells a terminal whose error it is; JSON already knows.
            assert!(!message.starts_with("trekr:"), "{args:?}: {message}");
            assert!(
                !message.contains("rev-parse"),
                "git's words, not ours: {message}"
            );
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(
                stderr.contains(message),
                "{args:?}: the message is on stderr too"
            );
        }
    }

    // Counting switched off is nothing recorded, not a mistake in the call.
    let off = trekr_env(&db, &dir, &["--usage"], &[("TREKR_USAGE", "off")]);
    assert_eq!(off.status.code(), Some(1));
    assert!(stdout(&off).contains("TREKR_USAGE=off"), "{off:?}");
    let off = trekr_env(&db, &dir, &["--usage", "-j"], &[("TREKR_USAGE", "off")]);
    assert_eq!(
        (off.status.code(), json(&off)),
        (Some(1), serde_json::json!([]))
    );

    // The mode is read off argv before clap parses it: a flag in front of
    // the bad one, or inside a cluster, still asks for JSON.
    for args in [
        &["-j", "--no-such-flag"][..],
        &["-jJ"],
        &["--json", "--no-such-flag", "--"],
    ] {
        let out = trekr(&db, &dir, args);
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert_eq!(json(&out)["kind"], "usage", "{args:?}");
    }
    // …but not after `--`, where nothing is a flag.
    let out = trekr(&db, &dir, &["--no-such-flag", "--", "-j"]);
    assert_eq!(out.status.code(), Some(64));
    assert_eq!(stdout(&out), "");

    // Asking about trekr is not an error, whatever the mode.
    for args in [
        &["--help"][..],
        &["--version"],
        &["-h", "--json"],
        &["--version", "-J"],
    ] {
        let out = trekr(&db, &dir, args);
        assert_eq!(out.status.code(), Some(0), "{args:?}");
        assert!(out.stderr.is_empty(), "{args:?}");
    }

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&plain);
}

/// Completions are generated from the parser, so they must not need a checkout
/// — and the directory below deliberately is not one, because every other
/// command refuses that.
#[test]
fn a_completion_script_is_generated_without_a_repository() {
    let (dir, db) = scratch("completions");
    let out = trekr(&db, &dir, &["--completions", "bash"]);

    assert_eq!(out.status.code(), Some(0), "generating a script succeeded");
    assert!(
        stdout(&out).contains("complete -F _trekr"),
        "the script registers the completion function: {}",
        stdout(&out)
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
        Some(74),
        "the store cannot be read: an error, not an empty answer"
    );
    let message = String::from_utf8_lossy(&refused.stderr);
    assert!(
        message.contains("upgrade trekr"),
        "and it says what to do: {message}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// Every CLI use is counted once its answer is out — by command, caller and
/// outcome — and `--usage` folds the counts back into which commands get used,
/// by whom, and how often they come back empty.
#[test]
fn usage_counts_each_command_by_caller_and_outcome() {
    let (dir, db) = scratch("usage");
    repo(&dir);
    let agent = [("CLAUDECODE", "1")];

    // Nothing counted yet is a definitive "no", not a failure.
    assert_eq!(trekr(&db, &dir, &["--usage"]).status.code(), Some(1));

    // Asked before anything was indexed: the answer an agent needs to hear.
    trekr_env(&db, &dir, &["--def", "widget.rb:7:5"], &agent);
    trekr_env(&db, &dir, &["--index"], &agent);
    trekr_env(&db, &dir, &["--def", "widget.rb:7:5", "--json"], &agent);
    trekr_env(&db, &dir, &["Widget#nope"], &agent);
    // No agent in the environment and no terminal: an unattributed pipe.
    trekr(&db, &dir, &["--ancestors", "Widget"]);

    let out = trekr(&db, &dir, &["--usage", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let rows = json(&out);
    let rows = rows.as_array().expect("an array of daily rows");
    let find = |feature: &str, outcome: &str| {
        rows.iter()
            .find(|r| r["feature"] == feature && r["outcome"] == outcome)
            .unwrap_or_else(|| panic!("no {feature}/{outcome} row in {rows:?}"))
    };
    assert_eq!(find("def", "not-indexed")["origin"], "claude-code");
    let def = find("def", "hit");
    assert_eq!(def["surface"], "cli");
    assert_eq!(
        def["flags"], "json,tree-built",
        "the first query after an index assembles the tree"
    );
    assert_eq!(find("ancestors", "hit")["flags"], "", "later ones map it");
    assert_eq!(def["count"], 1);
    assert!(def["latency"].as_str().unwrap().starts_with('<'));
    // The bare grammar is counted as what it dispatched to, marked as bare.
    assert_eq!(find("card", "empty")["flags"], "bare");
    assert_eq!(find("ancestors", "hit")["origin"], "piped");
    assert_eq!(find("index", "hit")["origin"], "claude-code");
    // `--usage` itself is not a use of the engine.
    assert!(!rows.iter().any(|r| r["feature"] == "usage"));
    // Counts, not content: no query, path or repository is kept.
    let text = serde_json::to_string(rows).unwrap();
    assert!(!text.contains("Widget") && !text.contains("widget.rb"));
    assert!(!text.contains(&dir.display().to_string()));

    let summary = stdout(&trekr(&db, &dir, &["--usage"]));
    assert!(summary.contains("command line"), "{summary}");
    assert!(summary.contains("claude-code"), "{summary}");
    let ndjson = stdout(&trekr(&db, &dir, &["--usage", "--ndjson", "--days", "1"]));
    assert_eq!(ndjson.lines().count(), rows.len(), "today holds every row");

    let _ = fs::remove_dir_all(&dir);
}

/// Asking trekr about itself is not a use of it; a call it could not parse is.
#[test]
fn help_and_version_are_not_counted_but_a_malformed_call_is() {
    let (dir, db) = scratch("usage-help");
    repo(&dir);
    for args in [&["--help"][..], &["--version"], &["-h"]] {
        assert_eq!(trekr(&db, &dir, args).status.code(), Some(0));
    }
    assert_eq!(
        trekr(&db, &dir, &["--usage"]).status.code(),
        Some(1),
        "nothing counted"
    );

    assert_eq!(
        trekr(&db, &dir, &["--no-such-flag"]).status.code(),
        Some(64)
    );
    let rows = json(&trekr(&db, &dir, &["--usage", "--json"]));
    assert_eq!(rows[0]["feature"], "invalid");
    assert_eq!(rows[0]["outcome"], "error:usage");
    // A failure is counted under the same `kind` its JSON error carries.
    trekr(&db, &dir, &["--symbols", "gone.rb"]);
    let rows = json(&trekr(&db, &dir, &["--usage", "--json"]));
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|r| r["feature"] == "symbols" && r["outcome"] == "error:not_found"),
        "{rows}"
    );

    // And `TREKR_USAGE=off` means off.
    trekr_env(&db, &dir, &["--no-such-flag"], &[("TREKR_USAGE", "off")]);
    let rows = json(&trekr(&db, &dir, &["--usage", "--json"]));
    assert_eq!(rows[0]["count"], 1);

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

    // A bare position is a --def, so it takes --explain too; nothing else does.
    let bare = stdout(&trekr(&db, &dir, &["app.rb:11:13", "--explain"]));
    assert_eq!(bare, explained);
    for args in [
        &["--refs", "ship", "--explain"][..],
        &["Widget#ship", "--explain"],
    ] {
        let out = trekr(&db, &dir, args);
        assert_eq!(out.status.code(), Some(64), "{args:?}");
        assert!(String::from_utf8_lossy(&out.stderr).contains("position"));
    }

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

    // `--status` shows the app, its gems counted, not listed; `--all` lists.
    let status = json(&trekr(&db, &app, &["--status", "--json"]));
    let rows = status["checkouts"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{status}");
    assert_eq!(rows[0]["kind"], "repo");
    assert_eq!(rows[0]["gems"]["count"], 2, "{status}");
    assert_eq!(rows[0]["gems"]["indexed"], 2, "{status}");
    assert_eq!(
        status["others"]["gems"], 0,
        "the app's gems are counted on its row"
    );
    let every = json(&trekr(&db, &app, &["--status", "--all", "--json"]));
    let kinds: Vec<&str> = every["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds.iter().filter(|k| **k == "gem").count(), 2, "{every}");
    let text = stdout(&trekr(&db, &app, &["--status"]));
    assert!(text.contains("+ 2 gems, all indexed"), "{text}");

    // The call lives in the `user` gem; the definition lives in `helper`.
    let spec = format!("{}:2:3", user.join("user.rb").display());
    let answer = json(&trekr(&db, &app, &["--def", &spec, "--json"]));
    assert_eq!(
        answer["status"], "resolved",
        "the sibling gem's method is reachable: {answer}"
    );
    assert_eq!(answer["definition"][0]["path"], "lib/helper.rb", "{answer}");
    assert!(
        answer["definition"][0]["root"]
            .as_str()
            .unwrap()
            .ends_with("helper-1.0.0"),
        "and it points at the gem that defines it: {answer}"
    );
    // An answer that depends on which app supplied the ancestors says which.
    assert_eq!(
        answer["context"].as_str(),
        app.canonicalize().unwrap().to_str(),
        "the answering context is disclosed"
    );

    // A gem is no git checkout: indexing it says how to refresh it instead,
    // and dropping it by its directory works.
    let gem = gems.join("gems/helper-1.0.0");
    let gem_dir = gem.to_str().unwrap();
    let out = trekr(&db, &app, &["--index", gem_dir]);
    assert_eq!(out.status.code(), Some(66));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is a gem") && stderr.contains("--drop"),
        "{stderr}"
    );
    let dropped = json(&trekr(&db, &app, &["--drop", gem_dir, "--json"]));
    assert_eq!(dropped["dropped"], true, "{dropped}");

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
}

/// Human output never prints an absolute `$HOME` path; `--json` always does.
///
/// One assertion over every text surface at once, because the rule is only
/// worth having if the *next* output surface cannot forget it. `HOME` is
/// pointed at the scratch directory so the fixture genuinely lives under it —
/// without that, every path here is under `/tmp` and the test passes by
/// accident.
#[test]
fn human_output_shortens_home_and_json_keeps_it_absolute() {
    let (dir, db) = scratch("tilde");
    repo(&dir);
    // Canonical, because git reports `/private/var/...` where `temp_dir()`
    // says `/var/...` on macOS — the same symlink DEC-026 audited.
    let real = dir.canonicalize().unwrap();
    let home = real.parent().unwrap().to_string_lossy().to_string();
    let vars = [("HOME", home.as_str())];
    let absolute = real.to_string_lossy().to_string();

    let indexed = trekr_env(&db, &dir, &["--index"], &vars);
    let text = String::from_utf8_lossy(&indexed.stdout);
    assert!(text.contains('~'), "index text should shorten HOME: {text}");
    assert!(
        !text.contains(&absolute),
        "index text should not print an absolute HOME path: {text}"
    );

    for args in [
        vec!["--status"],
        vec!["--def", "widget.rb:7:5"],
        vec!["--refs", "Widget#helper"],
    ] {
        let out = trekr_env(&db, &dir, &args, &vars);
        let text = String::from_utf8_lossy(&out.stdout);
        // Non-empty, so the absence of an absolute path is a real result and
        // not an error path printing nothing.
        assert!(!text.trim().is_empty(), "{args:?} printed nothing");
        assert!(
            !text.contains(&absolute),
            "{args:?} text printed an absolute HOME path: {text}"
        );
    }

    // The machine surface is the other half of the rule: a consumer that has
    // to expand `~` is one that will forget to.
    let json = trekr_env(&db, &dir, &["--status", "--json"], &vars);
    let text = String::from_utf8_lossy(&json.stdout);
    assert!(
        text.contains(&absolute),
        "--json must keep absolute paths: {text}"
    );
    assert!(!text.contains('~'), "--json must not shorten: {text}");
}

/// A checkout nobody indexed says so, instead of answering `residue`.
///
/// The old answer — "no indexed constant by that name" — reads as a finding
/// about the code when it is a fact about the setup, and the two call for
/// opposite reactions. Exit 2, because exit 1 is this tool's "a definitive
/// nothing" and would tell a script the question had been answered.
#[test]
fn an_unindexed_checkout_is_a_setup_problem_not_a_residue() {
    let (dir, db) = scratch("not-indexed");
    repo(&dir);

    for args in [
        vec!["--def", "widget.rb:7:5"],
        vec!["--refs", "Widget#helper"],
    ] {
        let out = trekr(&db, &dir, &args);
        assert_eq!(out.status.code(), Some(2), "{args:?} should not be exit 1");
        let text = String::from_utf8_lossy(&out.stderr);
        assert!(text.contains("not indexed"), "{args:?}: {text}");
        assert!(
            text.contains("--index"),
            "{args:?} should say what to run: {text}"
        );
    }

    let json = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json on stdout");
    assert_eq!(value["status"], "not_indexed");
    assert!(
        value["repo"]
            .as_str()
            .is_some_and(|r| r.ends_with("not-indexed"))
    );

    // And it stops being a setup problem the moment it is indexed.
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let out = trekr(&db, &dir, &["--def", "widget.rb:7:5"]);
    assert_eq!(out.status.code(), Some(0), "an indexed checkout answers");
}

/// A hand-typed column is a guess, so `--def` snaps to the nearest name on the
/// line — and says that it did.
#[test]
fn a_rough_column_snaps_to_the_nearest_name_and_discloses_it() {
    let (dir, db) = scratch("snap");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    // `    helper` — the name starts at column 5.
    let exact = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&exact.stdout).unwrap();
    assert_eq!(value["name"], "helper");
    assert!(
        value.get("snapped_to").is_none(),
        "an exact hit must not claim to have snapped: {value}"
    );

    // One column into the leading whitespace.
    let near = trekr(&db, &dir, &["--def", "widget.rb:7:3", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&near.stdout).unwrap();
    assert_eq!(value["name"], "helper");
    assert_eq!(value["snapped_to"]["name"], "helper");
    assert_eq!(value["snapped_to"]["col"], 5);
    assert_eq!(near.status.code(), Some(0));

    // The human surface says it in words, beside the answer.
    let told = stdout(&trekr(&db, &dir, &["--def", "widget.rb:7:3"]));
    assert!(
        told.contains("snapped_to  `helper` at column 5: no name at column 3"),
        "the snap must be told, and where: {told}"
    );

    // No column at all — the fully hand-typed case.
    let bare = trekr(&db, &dir, &["--def", "widget.rb:7", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&bare.stdout).unwrap();
    assert_eq!(value["name"], "helper");
    assert_eq!(value["snapped_to"]["col"], 5);

    // A line with several names discloses the ones it did not pick, so the
    // next query can be exact.
    let many = trekr(&db, &dir, &["--def", "widget.rb:1", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&many.stdout).unwrap();
    let alternatives = value["snapped_to"]["alternatives"]
        .as_array()
        .expect("alternatives listed");
    assert!(
        !alternatives.is_empty(),
        "`class Widget < Base` has more than one name: {value}"
    );
    assert!(alternatives.iter().all(|a| a["col"].is_number()));
}

/// The no-op skip must not swallow an edit that leaves the tree surface alone.
///
/// `surface_key` is deliberately blind to a body-only change — that is what
/// makes it a good staleness key for the tree layer (DEC-007). The file map is
/// not: the blob moved, its call sites moved with it, and `--refs` would point
/// at stale lines. So the skip is gated on `map_key`, which folds the blob oid,
/// and this pins the difference between the two.
#[test]
fn a_body_only_edit_is_still_written_even_though_the_surface_is_unchanged() {
    let (dir, db) = scratch("map-key");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    // Same definitions, same lines, different body — `helper` gains a call.
    let file = dir.join("widget.rb");
    let before = fs::read_to_string(&file).unwrap();
    let after = before.replace("  def helper\n  end", "  def helper\n    resize\n  end");
    assert_ne!(before, after, "the fixture must actually change");
    fs::write(&file, &after).unwrap();

    let out = trekr(&db, &dir, &["--index"]);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1 parsed"),
        "an edited blob must be re-parsed, not skipped: {text}"
    );

    // And the new call site is actually queryable, which is the point.
    let refs = trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&refs.stdout).unwrap();
    assert!(
        value["counts"]["confirmed"].as_u64().unwrap_or(0) >= 1,
        "the edit's new call should be found: {value}"
    );

    // Indexing again with nothing changed goes back to writing nothing.
    let again = trekr(&db, &dir, &["--index"]);
    let text = String::from_utf8_lossy(&again.stdout);
    assert!(
        text.contains("0 parsed"),
        "a true no-op parses nothing: {text}"
    );
}

/// DEC-035: a query probes git in O(1), refreshes the file it was asked about,
/// and discloses that the rest of the index may lag.
#[test]
fn a_query_refreshes_the_file_it_asks_about_and_says_the_rest_may_lag() {
    let (dir, db) = scratch("freshness");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    // Fresh: nothing to disclose.
    let clean = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&clean.stdout).unwrap();
    assert!(
        value.get("index").is_none(),
        "a fresh checkout says nothing: {value}"
    );

    // Push `helper` down two lines, and commit so git's index moves with it.
    let file = dir.join("widget.rb");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("class Widget < Base", "class Widget < Base\n  # a\n  # b"),
    )
    .unwrap();
    // A second file that nobody will ask about, to prove the refresh is bounded.
    fs::write(
        dir.join("other.rb"),
        "class Other\n  def only_here\n  end\nend\n",
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
            "edit",
        ],
    );

    let out = trekr(&db, &dir, &["--def", "widget.rb:9:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        value["index"]["stale"], true,
        "staleness disclosed: {value}"
    );
    assert_eq!(value["index"]["refreshed"], "widget.rb");
    assert_eq!(
        value["definition"][0]["line"], 14,
        "the answer must use the moved definition: {value}"
    );

    // Bounded: the file nobody asked about is still absent from the index.
    let other = trekr(&db, &dir, &["--refs", "Other#only_here", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&other.stdout).unwrap();
    assert!(
        value["definition"].as_array().is_none_or(|d| d.is_empty()),
        "a query must refresh one file, not the repo: {value}"
    );
}

/// A running `--index` holds the write lock for seconds. A read command
/// answers from what is committed and exits without waiting on it, and `--def`
/// says when the file it asked about could not be refreshed (DEC-065).
#[test]
fn read_commands_answer_and_exit_while_another_process_writes() {
    let (dir, db) = scratch("held-lock");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    // Move `helper` down two lines, and let git see it, so `--def` refreshes.
    let file = dir.join("widget.rb");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("class Widget < Base", "class Widget < Base\n  # a\n  # b"),
    )
    .unwrap();
    git(&dir, &["add", "-A"]);

    // The store and the usage counter, each held the way a writer holds it.
    let hold = |path: &Path| {
        let conn = rusqlite::Connection::open(path).unwrap();
        conn.execute_batch("BEGIN IMMEDIATE").unwrap();
        conn
    };
    let store = hold(&db);
    let usage = hold(&db.with_extension("usage.db"));

    let timed = |args: &[&str]| {
        let started = std::time::Instant::now();
        let out = trekr(&db, &dir, args);
        let elapsed = started.elapsed();
        assert!(out.status.success(), "{args:?}: {out:?}");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "{args:?} waited on the lock: {elapsed:?}"
        );
        out
    };
    timed(&["--status", "--json"]);
    timed(&["--ancestors", "Widget", "--json"]);
    timed(&["--refs", "Widget#helper", "--json"]);
    let value = json(&timed(&["--def", "widget.rb:9:5", "--json"]));
    assert_eq!(value["index"]["busy"], "widget.rb", "{value}");
    assert!(value["index"]["refreshed"].is_null(), "{value}");
    assert_eq!(
        value["definition"][0]["line"], 12,
        "answered from the committed index: {value}"
    );

    drop((store, usage));
    let value = json(&trekr(&db, &dir, &["--def", "widget.rb:9:5", "--json"]));
    assert_eq!(value["index"]["refreshed"], "widget.rb", "{value}");
    assert!(value["index"].get("busy").is_none(), "{value}");
    assert_eq!(value["definition"][0]["line"], 14, "{value}");
    let _ = fs::remove_dir_all(&dir);
}

/// The probe's blind spot, pinned rather than discovered later (DEC-035).
///
/// An edit that nothing has told git about does not move `.git/index`, so the
/// probe cannot see it and the answer is stale. That is the stated cost of an
/// O(1) check, and `--index` is the cure.
#[test]
fn an_edit_git_has_not_noticed_is_not_seen_by_the_probe() {
    let (dir, db) = scratch("blind-spot");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    let file = dir.join("widget.rb");
    let text = fs::read_to_string(&file).unwrap();
    fs::write(
        &file,
        text.replace("class Widget < Base", "class Widget < Base\n  # a\n  # b"),
    )
    .unwrap();

    let out = trekr(&db, &dir, &["--def", "widget.rb:9:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(
        value.get("index").is_none(),
        "the probe cannot see this, and must not claim it did: {value}"
    );

    // And an explicit index is the cure, as the DEC says.
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let out = trekr(&db, &dir, &["--def", "widget.rb:9:5", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["definition"][0]["line"], 14, "after --index: {value}");
}

/// `trekr <input>` dispatches on shape (DEC-036), and every shape speaks JSON.
#[test]
fn the_bare_grammar_dispatches_on_shape() {
    let (dir, db) = scratch("grammar");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    // A method: definition plus the tier counts, which is the whole reason the
    // grammar is not just an alias for a flag.
    let out = trekr(&db, &dir, &["Widget#helper", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["owner"], "Widget");
    assert_eq!(value["method"], "helper");
    assert_eq!(value["kind"], "definition");
    assert!(
        value["counts"]["confirmed"].as_u64().unwrap_or(0) >= 1,
        "{value}"
    );
    assert!(
        !value["definition"].as_array().unwrap().is_empty(),
        "{value}"
    );

    // A constant: definition plus what it inherits.
    let out = trekr(&db, &dir, &["Widget", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["fqn"], "Widget");
    assert!(
        value["ancestors"].as_array().is_some_and(|a| !a.is_empty()),
        "{value}"
    );

    // A position, with and without a column — both reach `--def`.
    for spec in ["widget.rb:7:5", "widget.rb:7"] {
        let out = trekr(&db, &dir, &[spec, "--json"]);
        let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(value["name"], "helper", "{spec}: {value}");
    }

    // The flags stay the explicit form — a script never has to rely on shape.
    let bare = trekr(&db, &dir, &["widget.rb:7:5", "--json"]);
    let flag = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    assert_eq!(
        bare.stdout, flag.stdout,
        "bare and --def must agree exactly"
    );

    // A shape it cannot name is refused, not guessed at.
    let out = trekr(&db, &dir, &["not a thing"]);
    assert_eq!(out.status.code(), Some(64));
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        text.contains("Expected"),
        "the refusal spells the shapes: {text}"
    );
}

/// `--dead` tiers candidates by what evidence was found, and never says "dead".
#[test]
fn dead_candidates_are_tiered_by_the_evidence_found() {
    let (dir, db) = scratch("dead");
    repo(&dir);
    fs::write(
        dir.join("thing.rb"),
        "class Thing\n  validate :check_it\n\n  def check_it\n  end\n\n  \
         def used_once\n  end\n\n  def never_used\n  end\n\n  \
         def run\n    used_once\n  end\n\n  def maybe\n  end\n\n  \
         private\n\n  def secret\n  end\nend\n",
    )
    .unwrap();
    fs::write(dir.join("caller.rb"), "something.maybe\n").unwrap();
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
            "add",
        ],
    );
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    let out = trekr(&db, &dir, &["--dead", "thing.rb", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let by_name: std::collections::HashMap<&str, &serde_json::Value> = value["candidates"]
        .as_array()
        .expect("candidates")
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c))
        .collect();

    assert_eq!(by_name["never_used"]["tier"], "unreferenced");
    // Whether deleting it can break a caller outside the checkout.
    assert_eq!(by_name["never_used"]["visibility"], "public");
    assert_eq!(by_name["secret"]["visibility"], "private");
    // Reached only by `validate :check_it` — its own tier, because it is both
    // the likeliest real candidate and the likeliest false positive.
    assert_eq!(by_name["check_it"]["tier"], "convention-only");
    assert_eq!(by_name["used_once"]["tier"], "single-caller");
    // The one call is named, and whether it certainly reaches the method.
    assert_eq!(by_name["used_once"]["caller"]["tier"], "confirmed");
    assert_eq!(by_name["used_once"]["caller"]["line"], 14);
    let reason = by_name["used_once"]["reason"].as_str().unwrap();
    assert!(reason.contains("run, is itself a candidate"), "{reason}");
    assert_eq!(by_name["maybe"]["caller"]["tier"], "possible");
    assert_eq!(by_name["maybe"]["confidence"], "lower");
    assert_eq!(by_name["maybe"]["caveat"], "untyped caller");
    assert_eq!(by_name["used_once"]["confidence"], "clear");
    assert!(
        by_name["maybe"]["reason"]
            .as_str()
            .unwrap()
            .contains("possible")
    );
    let text = stdout(&trekr(&db, &dir, &["--dead", "thing.rb"]));
    assert!(text.contains("one possible call, at caller.rb:1"), "{text}");
    // Named as Ruby's documentation names it, so a class method says so.
    assert!(text.contains("  Thing#never_used  "), "{text}");
    // Counted by tier, in text and JSON alike; a tier with none is a zero.
    assert!(
        text.contains("6 candidates in 1 file(s): 3 unreferenced, 1 convention-only, 2 single-caller (5 clear, 1 lower)"),
        "{text}"
    );
    assert_eq!(value["summary"]["candidates"], 6, "{value}");
    assert_eq!(value["summary"]["tiers"]["single-caller"], 2, "{value}");
    assert_eq!(value["summary"]["tiers"]["override"], 0, "{value}");
    assert_eq!(value["summary"]["confidence"]["lower"], 1, "{value}");
    assert!(text.contains("Thing#secret (private)"), "{text}");
    // The caller itself is used by nothing here, so it is reported too — but
    // never as anything stronger than a candidate.
    for candidate in value["candidates"].as_array().unwrap() {
        assert!(candidate.get("dead").is_none(), "nothing is asserted dead");
        assert!(candidate["confidence"].is_string(), "confidence is graded");
    }

    // A file whose dispatch is dynamic lowers confidence, and says which shape.
    fs::write(
        dir.join("dyn.rb"),
        "class Dyn\n  def hidden\n  end\n\n  def go(n)\n    send(n)\n  end\n\n  \
         class_eval \"def #{NAME * 2}; end\"\nend\n",
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
            "dyn",
        ],
    );
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let out = trekr(&db, &dir, &["--dead", "dyn.rb", "--json"]);
    let value: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    let hidden = value["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hidden")
        .expect("hidden is a candidate");
    assert_eq!(hidden["confidence"], "lower");
    let caveat = hidden["caveat"].as_str().unwrap();
    assert!(caveat.contains("send"), "{hidden}");
    // A string of code trekr could not read hides its calls too (DEC-132).
    assert!(caveat.contains("class_eval string"), "{hidden}");
}

/// Scopes in two checkouts are each weighed against their own checkout's
/// callers. The first path's checkout used to be the evidence for all of them,
/// so the answer turned on argument order.
#[test]
fn dead_weighs_each_scope_against_its_own_checkout() {
    let (base, db) = scratch("dead-two");
    let (one, two) = (base.join("one"), base.join("two"));
    let widget = "class Widget\n  def self.build\n  end\nend\n";
    for (dir, calls) in [(&one, 1), (&two, 3)] {
        fs::create_dir_all(dir).unwrap();
        git(dir, &["init", "-q"]);
        fs::write(dir.join("widget.rb"), widget).unwrap();
        let body = "    Widget.build\n".repeat(calls);
        fs::write(
            dir.join("main.rb"),
            format!("class Main\n  def go\n{body}  end\nend\n"),
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
        assert!(trekr(&db, dir, &["--index"]).status.success());
    }
    let (one_scope, two_scope) = (
        one.join("widget.rb").to_string_lossy().into_owned(),
        two.join("widget.rb").to_string_lossy().into_owned(),
    );
    for args in [[&one_scope, &two_scope], [&two_scope, &one_scope]] {
        let out = trekr(&db, &base, &["--dead", args[0], args[1], "--json"]);
        let value = json(&out);
        assert_eq!(value["scope"], 2, "{value}");
        let candidates = value["candidates"].as_array().unwrap();
        // One caller in `one`; three in `two`, which is not a candidate.
        assert_eq!(candidates.len(), 1, "{args:?}: {value}");
        assert_eq!(candidates[0]["tier"], "single-caller");
        assert_eq!(candidates[0]["singleton"], true);
        assert!(
            candidates[0]["root"].as_str().unwrap().ends_with("/one"),
            "{value}"
        );
        // Text names no checkout as "here", so "widget.rb" alone would be
        // read against the first scope's.
        let text = stdout(&trekr(&db, &base, &["--dead", args[0], args[1]]));
        assert!(text.contains("one/widget.rb:2  Widget.build"), "{text}");
    }

    // A file named twice, directly and through its directory, is one file.
    let dir = one.to_string_lossy().into_owned();
    let twice = json(&trekr(
        &db,
        &base,
        &["--dead", &one_scope, &dir, &one_scope, "--json"],
    ));
    assert_eq!(twice["scope"], 2, "widget.rb and main.rb: {twice}");
    let names: Vec<&str> = twice["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names.iter().filter(|n| **n == "build").count(),
        1,
        "{twice}"
    );
}

/// The index the LSP starts in the background prepares the tree snapshot its
/// next request would otherwise assemble; an index someone runs does not.
#[test]
fn only_a_background_index_prepares_the_tree() {
    let (dir, db) = scratch("prebuild");
    repo(&dir);
    let trees = db.with_extension("trees");
    let files = || fs::read_dir(&trees).map_or(0, |d| d.count());
    trekr(&db, &dir, &["--index"]);
    assert_eq!(
        files(),
        0,
        "a foreground index leaves the tree to the first query"
    );

    fs::write(dir.join("gadget.rb"), "class Gadget\nend\n").unwrap();
    let background = Command::new(env!("CARGO_BIN_EXE_trekr"))
        .arg("--index")
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_BACKGROUND", "1")
        .output()
        .unwrap();
    assert!(background.status.success());
    assert_eq!(files(), 1);
    let profiled = Command::new(env!("CARGO_BIN_EXE_trekr"))
        .args(["--ancestors", "Gadget"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_PROFILE", "1")
        .output()
        .unwrap();
    let profile = String::from_utf8_lossy(&profiled.stderr);
    assert!(
        profile.contains("snapshot-load") && !profile.contains("assemble"),
        "the query maps what the index prepared: {profile}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A query leaves its checkout's tree snapshot beside the store. Once the
/// index moves on without another query, `--gc` removes the one nothing names
/// any more — and never the current one.
#[test]
fn gc_removes_tree_snapshots_no_checkouts_index_names() {
    let (dir, db) = scratch("gc-trees");
    repo(&dir);
    let trees = db.with_extension("trees");
    let files = || fs::read_dir(&trees).map_or(0, |d| d.count());
    trekr(&db, &dir, &["--index"]);
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Widget"]).status.code(),
        Some(0)
    );
    assert_eq!(files(), 1, "the query wrote its snapshot");

    fs::write(dir.join("gadget.rb"), "class Gadget\nend\n").unwrap();
    trekr(&db, &dir, &["--index"]);
    let dry = trekr(&db, &dir, &["--gc", "--dry-run", "--json"]);
    assert_eq!(
        dry.status.code(),
        Some(0),
        "stale snapshots are something to collect"
    );
    let dry = json(&dry);
    assert_eq!(dry["snapshots"]["files"], 1, "{dry}");
    assert!(dry["snapshots"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(files(), 1, "a dry run removes nothing");

    let text = stdout(&trekr(&db, &dir, &["--gc"]));
    assert!(text.contains("collected 1 tree snapshots"), "{text}");
    assert_eq!(files(), 0);
    let again = trekr(&db, &dir, &["--gc", "--json"]);
    assert_eq!(again.status.code(), Some(1), "nothing left to collect");
    assert_eq!(json(&again)["snapshots"]["files"], 0);

    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Gadget"]).status.code(),
        Some(0)
    );
    assert_eq!(trekr(&db, &dir, &["--gc"]).status.code(), Some(1));
    assert_eq!(files(), 1, "the current snapshot stays");
    let _ = fs::remove_dir_all(&dir);
}

/// An app moves from one gem version to the next. The old version is kept while
/// it is recent, collected once it is not — along with the file only it had,
/// never the one both versions ship — and rebuilt the moment a lockfile names
/// it again.
#[test]
fn gc_collects_a_gem_version_no_bundle_names_and_an_index_brings_it_back() {
    let (app, db) = scratch("gc-app");
    let (gems, _) = scratch("gc-gems");
    repo(&app);
    let shared = "module Shared\n  def helpers\n  end\nend\n";
    for (gem, own) in [("widget-1.0.0", "Old"), ("widget-2.0.0", "New")] {
        let lib = gems.join(format!("gems/{gem}/lib"));
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join("shared.rb"), shared).unwrap();
        fs::write(
            lib.join("own.rb"),
            format!("class {own}\n  include Shared\nend\n"),
        )
        .unwrap();
    }
    let lock = |version: &str| {
        fs::write(
            app.join("Gemfile.lock"),
            format!("GEM\n  remote: https://rubygems.org/\n  specs:\n    widget ({version})\n"),
        )
        .unwrap();
    };
    let env = [("GEM_HOME", gems.to_str().unwrap())];
    let run = |args: &[&str]| trekr_env(&db, &app, args, &env);
    let blobs = || json(&run(&["--status", "--json"]))["totals"]["blobs"].clone();

    lock("1.0.0");
    run(&["--index"]);
    lock("2.0.0");
    run(&["--index"]);
    let old_root = gems.join("gems/widget-1.0.0").canonicalize().unwrap();
    assert_eq!(blobs(), 4, "the app, the shared file once, and each own.rb");

    let bare = run(&["--gc", "--older-than", "7"]);
    assert_eq!(bare.status.code(), Some(64), "7 what? refused, not guessed");

    // Seen seconds ago, so the default window spares it.
    let recent = run(&["--gc", "--dry-run", "--json"]);
    assert_eq!(recent.status.code(), Some(1), "nothing to collect");
    assert_eq!(json(&recent)["checkouts"], serde_json::json!([]));

    let dry = run(&["--gc", "--dry-run", "--older-than", "0", "--json"]);
    assert_eq!(dry.status.code(), Some(0));
    let dry = json(&dry);
    assert_eq!(
        dry["checkouts"],
        serde_json::json!([{
            "repo": old_root.to_str().unwrap(),
            "kind": "gem",
            "reason": "unclaimed",
            "last_seen": dry["checkouts"][0]["last_seen"],
        }]),
        "exactly the version the app moved past: {dry}"
    );
    assert!(dry["last_seen"].is_null() && dry["checkouts"][0]["last_seen"].is_i64());
    for key in ["files", "blobs", "facts", "reclaimed_bytes", "db_bytes"] {
        assert!(dry[key].is_i64(), "{key}: {dry}");
    }
    assert_eq!(dry["dry_run"], true);
    assert_eq!(blobs(), 4, "a dry run removes nothing");

    let done = json(&run(&["--gc", "--older-than", "0", "--vacuum", "--json"]));
    assert_eq!(
        (done["files"].clone(), done["blobs"].clone()),
        (2.into(), 1.into())
    );
    assert_eq!(done["vacuumed"], true);
    assert_eq!(
        blobs(),
        3,
        "own.rb went; shared.rb is still mapped by 2.0.0"
    );
    let chain = json(&run(&["--ancestors", "New", "--json"]));
    assert!(chain["ancestors"].to_string().contains("Shared"), "{chain}");

    // Back to 1.0.0: the next index finds it missing and reads it again.
    lock("1.0.0");
    let again = json(&run(&["--index", "--json"]));
    assert_eq!(again["gems"]["indexed"], 1, "{again}");
    let chain = json(&run(&["--ancestors", "Old", "--json"]));
    assert_eq!(chain["status"], "resolved", "{chain}");
    assert!(chain["ancestors"].to_string().contains("Shared"), "{chain}");

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
}

/// Start trekr without waiting for it, so several can race for one store.
fn spawn_trekr(db: &Path, cwd: &Path, args: &[&str]) -> std::process::Child {
    neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(args)
        .current_dir(cwd)
        .env("TREKR_DB", db)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn trekr")
}

fn reset(db: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = fs::remove_file(format!("{}{suffix}", db.display()));
    }
}

fn count(db: &Path, sql: &str) -> i64 {
    rusqlite::Connection::open(db)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

/// A store an older trekr left: a schema this one does not speak.
fn old_store(db: &Path) {
    reset(db);
    rusqlite::Connection::open(db)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE checkout (x); PRAGMA user_version = 1;",
        )
        .unwrap();
}

#[test]
fn processes_opening_an_old_store_at_once_rebuild_it_once() {
    let (dir, db) = scratch("old-race");
    repo(&dir);
    // What a complete rebuild leaves, to hold each raced one to.
    let (_, reference) = scratch("old-race-ref");
    trekr(&reference, &dir, &["--status"]);
    let objects = "SELECT COUNT(*) FROM sqlite_master";
    for round in 0..6 {
        old_store(&db);
        let racers: Vec<_> = (0..4)
            .map(|i| {
                let args: &[&str] = if i % 2 == 0 {
                    &["--status", "--json"]
                } else {
                    &["--refs", "Widget#resize", "--json"]
                };
                spawn_trekr(&db, &dir, args)
            })
            .collect();
        for racer in racers {
            let out = racer.wait_with_output().unwrap();
            let stderr = String::from_utf8_lossy(&out.stderr);
            assert!(stderr.is_empty(), "round {round}: {stderr}");
        }
        assert_eq!(
            count(&db, objects),
            count(&reference, objects),
            "round {round}"
        );
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM upgrade"),
            1,
            "round {round}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_checkout_missing_after_an_upgrade_says_so() {
    let (dir, db) = scratch("upgraded");
    repo(&dir);
    old_store(&db);
    let out = trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let answer = json(&out);
    assert_eq!(answer["status"], "not_indexed");
    let reason = answer["reason"].as_str().unwrap();
    assert!(reason.contains("v1"), "{reason}");
    // `--status` says why it is empty too, rather than looking never used.
    let out = trekr(&db, &dir, &["--status", "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let reason = json(&out)["reason"].as_str().unwrap().to_string();
    assert!(reason.contains("v1"), "{reason}");

    // Once anything is indexed again, "not indexed" is about the checkout.
    let (other, _) = scratch("upgraded-other");
    repo(&other);
    assert!(trekr(&db, &other, &["--index"]).status.success());
    let answer = json(&trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]));
    let reason = answer["reason"].as_str().unwrap();
    assert!(!reason.contains("format changed"), "{reason}");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
}

#[test]
fn concurrent_index_runs_over_shared_content_both_land() {
    let (dir, db) = scratch("index-race");
    repo(&dir);
    for i in 0..20 {
        fs::write(
            dir.join(format!("part_{i}.rb")),
            format!("class Part{i}\n  def run\n  end\nend\n"),
        )
        .unwrap();
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
            "parts",
        ],
    );
    let clone = dir.with_extension("clone");
    let _ = fs::remove_dir_all(&clone);
    git(&dir, &["clone", "-q", ".", clone.to_str().unwrap()]);

    for round in 0..6 {
        reset(&db);
        let racers = [&dir, &clone].map(|root| spawn_trekr(&db, root, &["--index", "--no-gems"]));
        for racer in racers {
            let out = racer.wait_with_output().unwrap();
            assert!(
                out.status.success(),
                "round {round}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        assert_eq!(
            count(&db, "SELECT COUNT(*) FROM checkout"),
            2,
            "round {round}"
        );
        assert_eq!(
            count(
                &db,
                "SELECT COUNT(*) FROM file WHERE blob_id NOT IN (SELECT id FROM blob)"
            ),
            0,
            "round {round}: a file row points at no blob"
        );
        assert_eq!(count(&db, "SELECT COUNT(*) FROM blob"), 21, "round {round}");
    }
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&clone);
}

#[test]
fn drop_forgets_the_checkouts_tree_snapshots() {
    let (dir, db) = scratch("drop-trees");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);
    trekr(&db, &dir, &["--ancestors", "Widget"]);
    let trees = db.with_extension("trees");
    let snapshots = || fs::read_dir(&trees).map_or(0, |d| d.count());
    assert!(snapshots() > 0, "a query leaves a snapshot to drop");
    assert!(trekr(&db, &dir, &["--drop"]).status.success());
    assert_eq!(snapshots(), 0);
    let _ = fs::remove_dir_all(&dir);
}
