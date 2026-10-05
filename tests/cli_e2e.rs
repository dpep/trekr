//! End-to-end: the built binary, an isolated database, a real git repo.
//!
//! Behavior gets checked here rather than by hand-running `trekr`, so a
//! regression fails CI instead of being noticed later.

#![allow(
    clippy::disallowed_methods,
    reason = "a test reads its own fixtures and scratch files"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

mod support;

use support::{fixture_home, git, git_only};

/// A scratch repo and database for one test (see `support::scratch`), whose
/// checkout runs on the fixture's Ruby.
fn scratch(label: &str) -> (PathBuf, PathBuf) {
    let (dir, db) = support::scratch(label);
    fs::write(dir.join(".ruby-version"), "9.8.7\n").unwrap();
    (dir, db)
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

/// A command run as `support::neutral` says, on the fixture's Ruby. A test
/// about another Ruby stages one, under a home of its own.
fn neutral(mut command: Command) -> Command {
    support::neutral(&mut command, fixture_home());
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
    // A path inside the checkout indexes all of it, and says so.
    let within = stdout(&trekr(&db, &dir, &["--index", "widget.rb", "--no-gems"]));
    assert!(
        within.starts_with("indexed the checkout containing "),
        "{within}"
    );
    let whole = stdout(&trekr(&db, &dir, &["--index", "--no-gems"]));
    assert!(!whole.contains("containing"), "{whole}");
    assert!(!whole.contains("repeat"), "{whole}");
    // Two files with the same bytes are one blob, and text says why.
    fs::copy(dir.join("widget.rb"), dir.join("copy.rb")).unwrap();
    let copied = stdout(&trekr(&db, &dir, &["--index", "--no-gems"]));
    assert!(
        copied.contains("2 files, 1 blobs (1 file repeats another's bytes)"),
        "{copied}"
    );
    fs::remove_file(dir.join("copy.rb")).unwrap();
    trekr(&db, &dir, &["--index", "--no-gems"]);

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
        // A variable: its writes, and its mentions.
        json(&trekr(&db, &dir, &["--def", "widget.rb:6:14", "--json"])),
        json(&trekr(&db, &dir, &["--refs", "widget.rb:6:14", "--json"])),
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

/// A template's answers that land in another file — a view's `@ivar` in
/// its controller, a `render`'s partial — write paths as every other answer
/// does: relative, with the root beside them.
#[test]
fn a_templates_answers_write_paths_relative_to_their_root() {
    let (dir, db) = scratch("tmplpaths");
    git(&dir, &["init", "-q"]);
    for (path, text) in [
        (
            "app/controllers/widgets_controller.rb",
            "class WidgetsController\n  def show\n    @title = \"w\"\n  end\nend\n",
        ),
        (
            "app/views/widgets/show.html.erb",
            "<%= @title %>\n<%= render \"row\" %>\n",
        ),
        ("app/views/widgets/_row.html.erb", "<li></li>\n"),
    ] {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, text).unwrap();
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
    trekr(&db, &dir, &["--index"]);
    let root = fs::canonicalize(&dir)
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let view = "app/views/widgets/show.html.erb";
    for (at, want) in [
        ("1:6", "app/controllers/widgets_controller.rb"),
        ("2:15", "app/views/widgets/_row.html.erb"),
    ] {
        let answer = json(&trekr(
            &db,
            &dir,
            &["--def", &format!("{view}:{at}"), "--json"],
        ));
        let site = &answer["definition"][0];
        assert_eq!(site["path"], want, "{answer}");
        assert_eq!(site["root"], serde_json::json!(root), "{answer}");
    }
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
    let status = json(&trekr(&db, &dir, &["--status", "--all", "--json"]));
    let repos = status["checkouts"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["kind"] == "repo")
        .count();
    assert_eq!(repos, 2, "and the Ruby's stdlib they share: {status}");
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
            "index-rebuild",
            "file-map",
            "commit",
            "gem-scan",
            "rbs",
            "analyze",
            "tree"
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

/// Without a lockfile, the highest version is picked from one Ruby's gems:
/// another Ruby's newer copy was never on this load path (DEC-152).
#[test]
fn with_no_lockfile_only_the_active_rubys_gems_are_picked() {
    let (dir, db) = scratch("declared-ruby");
    repo(&dir);
    let (home, _) = scratch("declared-ruby-home");
    let current = home.join(".rvm/gems/ruby-3.4.9");
    for gems in [
        current.join("gems/widget-0.2.0/lib"),
        home.join(".gem/ruby/3.3.0/gems/widget-0.3.0/lib"),
    ] {
        fs::create_dir_all(&gems).unwrap();
        fs::write(gems.join("widget.rb"), "module Widget\nend\n").unwrap();
    }
    fs::write(
        dir.join("app.gemspec"),
        "Gem::Specification.new do |s|\n  s.add_dependency \"widget\"\nend\n",
    )
    .unwrap();
    // No `ruby` on `PATH`, so no Ruby's stdlib is chosen to search first
    // (DEC-291): a system Ruby in `/usr/bin` would be, on some machines.
    let path = git_only();
    let pick = |vars: &[(&str, &str)]| {
        let mut env = vec![
            ("HOME", home.to_str().unwrap()),
            ("GEM_PATH", ""),
            ("PATH", path.to_str().unwrap()),
        ];
        env.extend_from_slice(vars);
        json(&trekr_env(&db, &dir, &["--index", "--json"], &env))["gems"].clone()
    };

    let gems = pick(&[("GEM_HOME", current.to_str().unwrap())]);
    assert_eq!(
        gems["picked"],
        serde_json::json!(["widget 0.2.0"]),
        "{gems}"
    );
    assert!(
        gems["ruby"].as_str().unwrap().contains("GEM_HOME"),
        "{gems}"
    );

    // The checkout's own `.ruby-version` names the Ruby before the shell does.
    fs::write(dir.join(".ruby-version"), "3.3.1\n").unwrap();
    let gems = pick(&[("GEM_HOME", current.to_str().unwrap())]);
    assert_eq!(
        gems["picked"],
        serde_json::json!(["widget 0.3.0"]),
        "{gems}"
    );
    assert!(gems["ruby"].as_str().unwrap().contains("3.3.1"), "{gems}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
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
    assert_eq!(
        answer["gems"]["picked"],
        serde_json::json!(["widget 0.2.0"]),
        "what trekr chose is listed"
    );
    let text = stdout(&trekr(&db, &dir, &["--index"]));
    assert!(text.contains("highest installed version"), "{text}");
    assert!(text.contains("picked: widget 0.2.0"), "{text}");

    let _ = fs::remove_dir_all(&dir);
}

/// A gem's base class may reach what a subclass in the app defines by a
/// name no call writes: a drop's `public_send(key)` runs its methods, and a
/// dashboard's `self.class::ATTRS` reads its constant. `--dead` reads the
/// gem's file for each and grades the row lower; a gem's mixin, which every
/// model has, is not read so.
#[test]
fn dead_reads_what_a_gems_base_class_reaches_by_name() {
    let (dir, db) = scratch("dead-gem-base");
    repo(&dir);
    let gems = dir.join("vendor/bundle/ruby/3.3.0/gems");
    for (gem, file, source) in [
        (
            "dropkit-1.0.0",
            "dropkit.rb",
            "module Dropkit\n  class Drop\n    def invoke_drop(key)\n      public_send(key)\n    end\n  end\nend\n",
        ),
        (
            "boardkit-1.0.0",
            "boardkit.rb",
            "module Boardkit\n  class Base\n    def attrs\n      self.class::ATTRS\n    end\n  end\nend\n",
        ),
    ] {
        fs::create_dir_all(gems.join(gem).join("lib")).unwrap();
        fs::write(gems.join(gem).join("lib").join(file), source).unwrap();
    }
    fs::write(
        dir.join("Gemfile.lock"),
        concat!(
            "GEM\n",
            "  remote: https://rubygems.org/\n",
            "  specs:\n",
            "    boardkit (1.0.0)\n",
            "    dropkit (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  boardkit\n",
            "  dropkit\n",
        ),
    )
    .unwrap();
    fs::write(
        dir.join("widget.rb"),
        "class WidgetDrop < Dropkit::Drop\n  def title\n  end\nend\n\n\
         class WidgetBoard < Boardkit::Base\n  ATTRS = [].freeze\n  STALE = 1\n\n  def unused\n  end\nend\n\n\
         WidgetDrop\nWidgetBoard\n",
    )
    .unwrap();
    trekr(&db, &dir, &["--index"]);
    let dead = json(&trekr(&db, &dir, &["--dead", "widget.rb", "--json"]));
    let caveat = |name: &str| {
        dead["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["name"] == name)
            .map(|row| row["caveat"].as_str().unwrap_or_default().to_string())
            .unwrap_or_else(|| panic!("no row {name}: {dead}"))
    };
    assert!(caveat("title").contains("computes"), "{dead}");
    assert!(caveat("ATTRS").contains("read on a value"), "{dead}");
    assert!(
        caveat("STALE").contains("DEC-450"),
        "only the blanket rule: {dead}"
    );
    assert!(!caveat("unused").contains("computes"), "{dead}");
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
    // Between them in the bundle's stream: a gem with no `lib/`, and one
    // whose only file is a blob the first already parsed.
    let gems = dir.join("vendor/bundle/ruby/3.3.0/gems");
    fs::create_dir_all(gems.join("empty-1.0.0")).unwrap();
    fs::create_dir_all(gems.join("gamma-1.0.0/lib")).unwrap();
    fs::write(gems.join("gamma-1.0.0/lib/shared.rb"), shared).unwrap();
    fs::write(
        dir.join("Gemfile.lock"),
        concat!(
            "GEM\n",
            "  remote: https://rubygems.org/\n",
            "  specs:\n",
            "    alpha (1.0.0)\n",
            "    empty (1.0.0)\n",
            "    gamma (1.0.0)\n",
            "    beta (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  alpha\n",
            "  empty\n",
            "  gamma\n",
            "  beta\n",
        ),
    )
    .unwrap();

    let out = trekr(&db, &dir, &["--index", "--profile", "--json"]);
    assert_eq!(json(&out)["gems"]["indexed"], 3);
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

/// A row set streams under `--ndjson`: each row on its own line, as `--json`
/// holds it, then one `{"answer": …}` line — the `--json` answer without its
/// rows, and how many there were.
#[test]
fn ndjson_streams_a_row_set_one_row_per_line_then_the_answer() {
    let (dir, db) = scratch("ndjson-rows");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);

    let lines = |args: &[&str]| -> Vec<serde_json::Value> {
        stdout(&trekr(&db, &dir, args))
            .lines()
            .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")))
            .collect()
    };
    // (ndjson args, the --json answer's row key, or `None` for a bare array)
    let cases: [(&[&str], Option<&str>); 5] = [
        (&["--refs", "Widget#helper"], Some("references")),
        (&["--refs", "Widget#nothing"], Some("references")),
        (&["--dead", "widget.rb"], Some("candidates")),
        (&["--refs", "helper"], None),
        (&["--symbols", "widget.rb"], None),
    ];
    for (args, key) in cases {
        let whole = json(&trekr(&db, &dir, &[args, &["--json"]].concat()));
        let streamed = lines(&[args, &["--ndjson"]].concat());
        let (last, rows) = streamed.split_last().unwrap_or_else(|| panic!("{args:?}"));
        let want_rows = match key {
            Some(key) => whole[key].clone(),
            None => whole.clone(),
        };
        assert_eq!(
            serde_json::Value::from(rows.to_vec()),
            want_rows,
            "{args:?}: the rows, as --json holds them"
        );
        assert!(
            rows.iter().all(|row| row.get("answer").is_none()),
            "{args:?}"
        );
        let mut head = match key {
            Some(key) => {
                let mut head = whole.clone();
                head.as_object_mut().unwrap().remove(key);
                head
            }
            None => serde_json::json!({}),
        };
        head["rows"] = rows.len().into();
        assert_eq!(last, &serde_json::json!({ "answer": head }), "{args:?}");
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

/// A model's card carries its table from the app's SQL dump: the columns
/// with their types, null and defaults, the key and the indexes. A class that
/// is not a model has no `table` at all.
#[test]
fn a_models_card_carries_its_table() {
    let (dir, db) = scratch("model-card");
    git(&dir, &["init", "-q"]);
    fs::create_dir_all(dir.join("db")).unwrap();
    fs::write(
        dir.join("app.rb"),
        "module ActiveRecord\n  class Base\n  end\nend\n\
         class Widget < ActiveRecord::Base\nend\nclass Gadget\nend\n",
    )
    .unwrap();
    fs::write(
        dir.join("db/structure.sql"),
        "CREATE TABLE public.widgets (\n    id bigint NOT NULL,\n    \
         name character varying(60) DEFAULT 'x'::character varying NOT NULL\n);\n\
         ALTER TABLE ONLY public.widgets ADD CONSTRAINT widgets_pkey PRIMARY KEY (id);\n\
         CREATE UNIQUE INDEX index_widgets_on_name ON public.widgets USING btree (name);\n",
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

    let card = json(&trekr(&db, &dir, &["Widget", "--json"]));
    let table = &card["table"];
    assert_eq!(table["name"], "widgets", "{card}");
    assert_eq!(table["path"], "db/structure.sql", "{card}");
    assert_eq!(table["primary_key"], serde_json::json!(["id"]), "{card}");
    assert_eq!(
        table["columns"][1],
        serde_json::json!({
            "name": "name",
            "type": "character varying(60)",
            "class": "String",
            "null": false,
            "default": "'x'::character varying",
            "line": 3,
        }),
        "{card}"
    );
    assert_eq!(
        table["indexes"],
        serde_json::json!([{"columns": ["name"], "unique": true}]),
        "{card}"
    );
    let text = stdout(&trekr(&db, &dir, &["Widget"]));
    assert!(
        text.contains("table widgets · db/structure.sql:1 · 2 columns"),
        "{text}"
    );
    let card = json(&trekr(&db, &dir, &["Gadget", "--json"]));
    assert!(card.get("table").is_none(), "not a model: {card}");

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
    assert!(
        root.contains(".core/rbs-"),
        "one directory per Ruby's signatures: {root}"
    );
    assert!(text.contains("/String.rb:"), "{text}");
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
fn refs_text_folds_an_initializes_untyped_news_into_one_line() {
    let (dir, db) = scratch("refsuntypednew");
    git(&dir, &["init", "-q"]);
    fs::write(
        dir.join("app.rb"),
        concat!(
            "class Widget
", // 1
            "  def initialize(a); end\n", // 2
            "end
",                        // 3
            "Widget.new(1)
",              // 4  confirmed
            "klass.new(2)
",               // 5  untyped
            "other.new(3)
",               // 6  untyped
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

    let text = stdout(&trekr(&db, &dir, &["--refs", "Widget#initialize"]));
    assert!(text.contains("app.rb:4:"), "{text}");
    assert!(!text.contains("app.rb:5:"), "{text}");
    assert!(
        text.contains("2 untyped x.new (possible) — --json lists them"),
        "{text}"
    );
    // JSON keeps every row.
    let answer = json(&trekr(
        &db,
        &dir,
        &["--refs", "Widget#initialize", "--json"],
    ));
    assert_eq!(answer["references"].as_array().unwrap().len(), 3);

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

    // Never indexed: no answer yet, as a query says (DEC-170).
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(2));
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
    // A store whose directory would be made under a regular file.
    fs::write(dir.join("blocker"), "").unwrap();
    let unopenable = dir.join("blocker/store/trekr.db");
    let unopenable = unopenable.to_string_lossy().into_owned();

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
            &["--symbols", "/dev/zero"],
            &[],
            64,
            "usage",
            "not a regular file",
        ),
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
        (
            &dir,
            &["--status"],
            &[("TREKR_DB", &unopenable)],
            74,
            "database",
            "trekr store",
        ),
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
    // the bad one or behind it, or inside a cluster, still asks for JSON.
    for args in [
        &["-j", "--no-such-flag"][..],
        &["--no-such-flag", "--json"],
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
/// binary is *newer* than the database. An older binary meeting a newer
/// database keeps a store of its own beside it instead (DEC-300): refusing
/// failed every command until someone upgraded, and dropping would ping-pong.
#[test]
fn an_older_binary_keeps_its_own_store_beside_a_newer_one() {
    let (dir, db) = scratch("newerdb");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "PRAGMA user_version = 9999; \
             INSERT OR REPLACE INTO meta (key, value) VALUES ('schema_by', '9.9.9');",
        )
        .unwrap();
    let newer = fs::read(&db).unwrap();

    let status = trekr(&db, &dir, &["--status", "--json"]);
    let said = String::from_utf8_lossy(&status.stderr);
    assert!(said.contains("written by a newer trekr (9.9.9)"), "{said}");
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).expect("JSON");
    assert_eq!(json["status"], "not_indexed", "its own store starts empty");
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let def = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    assert!(def.status.success(), "{def:?}");
    assert!(
        !String::from_utf8_lossy(&def.stderr).contains("newer"),
        "said once"
    );
    assert_eq!(
        fs::read(&db).unwrap(),
        newer,
        "the newer store is untouched"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// Files in the store's directory whose names contain `part`.
fn beside(db: &Path, part: &str) -> Vec<String> {
    fs::read_dir(db.parent().unwrap())
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.contains(part) && !n.ends_with("-wal") && !n.ends_with("-shm"))
        .collect()
}

/// A store trekr can't use is set aside and rebuilt, and the command answers
/// as it would on a first run (DEC-300).
fn rebuilt_on_status(db: &Path, dir: &Path, said: &str) {
    let status = trekr(db, dir, &["--status", "--json"]);
    let err = String::from_utf8_lossy(&status.stderr);
    assert_eq!(err.matches(said).count(), 1, "{err}");
    assert!(
        err.contains("trekr.db.broken-"),
        "names the old file: {err}"
    );
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).expect("JSON");
    assert_eq!(json["status"], "not_indexed", "{json}");
    assert_eq!(beside(db, ".broken-").len(), 1);
    assert!(trekr(db, dir, &["--index"]).status.success());
    let def = trekr(db, dir, &["--def", "widget.rb:7:5", "--json"]);
    assert!(def.status.success(), "{def:?}");
}

#[test]
fn a_store_that_is_not_a_database_is_rebuilt() {
    let (dir, db) = scratch("garbage");
    repo(&dir);
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, "not a database ".repeat(1000)).unwrap();
    rebuilt_on_status(&db, &dir, "couldn't be read");
    let status = trekr(&db, &dir, &["--status", "--json"]);
    let json: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert!(
        json["status"] != "not_indexed",
        "rebuilt and indexed: {json}"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_truncated_store_is_rebuilt() {
    let (dir, db) = scratch("truncated");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);
    let len = fs::metadata(&db).unwrap().len();
    fs::File::options()
        .write(true)
        .open(&db)
        .unwrap()
        .set_len(len / 2)
        .unwrap();
    rebuilt_on_status(&db, &dir, "couldn't be read");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_rebuild_that_fails_is_set_aside_and_rebuilt() {
    let (dir, db) = scratch("failedrebuild");
    repo(&dir);
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    // an old store whose drop fails midway: a view where a table was
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE def (x); CREATE VIEW checkout AS SELECT 1; PRAGMA user_version = 3;",
        )
        .unwrap();
    rebuilt_on_status(&db, &dir, "couldn't be upgraded (from v3");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn concurrent_commands_on_a_broken_store_rebuild_it_once() {
    let (dir, db) = scratch("concurrent");
    repo(&dir);
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    fs::write(&db, "not a database ".repeat(1000)).unwrap();
    let children: Vec<_> = (0..6)
        .map(|_| {
            neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
                .args(["--status", "--json"])
                .current_dir(&dir)
                .env("TREKR_DB", &db)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut told = 0;
    for child in children {
        let out = child.wait_with_output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).expect("JSON");
        assert_eq!(json["status"], "not_indexed", "{json}");
        told += String::from_utf8_lossy(&out.stderr)
            .matches("couldn't be read")
            .count();
    }
    assert_eq!(told, 1, "one process set it aside and said so");
    assert_eq!(beside(&db, ".broken-").len(), 1);
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

    // Asked not to index before anything was: the answer an agent needs to
    // hear. And a query that indexed first says so.
    trekr_env(&db, &dir, &["--def", "widget.rb:7:5", "--no-index"], &agent);
    trekr_env(&db, &dir, &["--refs", "Widget#helper"], &agent);
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
    assert_eq!(find("def", "not-indexed")["flags"], "no-index");
    assert_eq!(find("refs", "hit")["flags"], "indexed");
    let def = find("def", "hit");
    assert_eq!(def["surface"], "cli");
    assert_eq!(
        def["flags"], "json",
        "the first query after an index maps the tree the index prepared"
    );
    assert_eq!(
        find("ancestors", "hit")["flags"],
        "",
        "and so do later ones"
    );
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
    assert_eq!(
        ndjson.lines().count(),
        rows.len() + 1,
        "today holds every row, then the answer"
    );

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

/// A gem pinned to git is checked out once per revision under
/// `bundler/gems/<repo>-<sha>/`, and a monorepo's gems are subdirectories
/// of that checkout, each with its gemspec (DEC-150).
fn git_monorepo_app(label: &str) -> (PathBuf, PathBuf, PathBuf) {
    let (app, db) = scratch(label);
    let (gems, _) = scratch(&format!("{label}-gems"));
    let checkout = gems.join("bundler/gems/kit-abc123def456");
    for (gem, file, body) in [
        (
            "kit_core",
            "kit_core/helpers.rb",
            "module KitCore\n  module Helpers\n    def core_help\n    end\n  end\nend\n",
        ),
        (
            "kit_web",
            "kit_web/base.rb",
            "module KitWeb\n  class Base\n    include KitCore::Helpers\n\n    def render_it\n      core_help\n    end\n  end\nend\n",
        ),
    ] {
        let lib = checkout.join(gem).join("lib");
        fs::create_dir_all(lib.join(gem)).unwrap();
        fs::write(checkout.join(gem).join(format!("{gem}.gemspec")), "").unwrap();
        fs::write(lib.join(file), body).unwrap();
    }
    // Bundler's checkout is a clone: it has a `.git` of its own.
    git(&checkout, &["init", "-q"]);

    git(&app, &["init", "-q"]);
    fs::create_dir_all(app.join("engines/billing")).unwrap();
    fs::write(
        app.join("page.rb"),
        "class Page < KitWeb::Base\n  def show\n    render_it\n  end\nend\n",
    )
    .unwrap();
    fs::write(
        app.join("Gemfile.lock"),
        concat!(
            "GIT\n",
            "  remote: https://github.com/example/kit.git\n",
            "  revision: abc123def4567890abc123def4567890abc12345\n",
            "  specs:\n",
            "    kit_core (0.1.0)\n",
            "    kit_web (0.1.0)\n",
            "      kit_core\n",
            "\n",
            "GIT\n",
            "  remote: https://github.com/example/gone.git\n",
            "  revision: 0000000000000000000000000000000000000000\n",
            "  specs:\n",
            "    gone (1.0.0)\n",
            "\n",
            "PATH\n",
            "  remote: engines/billing\n",
            "  specs:\n",
            "    billing (1.0.0)\n",
            "\n",
            "DEPENDENCIES\n",
            "  kit_web!\n",
        ),
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
    (app, db, gems)
}

#[test]
fn a_git_monorepos_gems_are_indexed_from_their_checkout() {
    let (app, db, gems) = git_monorepo_app("gitgems");
    let env = [("GEM_HOME", gems.to_str().unwrap())];

    let index = json(&trekr_env(&db, &app, &["--index", "--json"], &env));
    let report = &index["gems"];
    assert_eq!(report["found"], 2, "{index}");
    assert_eq!(report["from_git"], 2, "{index}");
    assert_eq!(report["from_path"], 1, "{index}");
    assert!(report.get("missing").is_none(), "{index}");
    let why = report["unlocated"][0]["why"].as_str().unwrap();
    assert!(
        why.contains("checkout not found") && why.contains("gone-000000000000"),
        "a revision nobody checked out says where it looked: {index}"
    );

    let text = stdout(&trekr_env(&db, &app, &["--index"], &env));
    assert!(text.contains("2 from git"), "{text}");
    assert!(!text.contains("not installed"), "{text}");

    // A call in the app lands in the gem's subdirectory, not the checkout.
    let answer = json(&trekr(&db, &app, &["--def", "page.rb:3:5", "--json"]));
    assert_eq!(answer["status"], "resolved", "{answer}");
    assert!(
        answer["definition"][0]["root"]
            .as_str()
            .unwrap()
            .ends_with("kit-abc123def456/kit_web"),
        "{answer}"
    );

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
}

/// Bundler's checkout of a git gem has a `.git`, which made git's toplevel —
/// the whole clone, never indexed — the context for any position in it.
/// It is a gem of the app that bundles it, like any other (DEC-150).
#[test]
fn a_position_in_a_git_gem_answers_from_the_app() {
    let (app, db, gems) = git_monorepo_app("gitgem-pos");
    let env = [("GEM_HOME", gems.to_str().unwrap())];
    assert!(trekr_env(&db, &app, &["--index"], &env).status.success());

    let web = gems.join("bundler/gems/kit-abc123def456/kit_web");
    let spec = format!("{}:6:7", web.join("lib/kit_web/base.rb").display());
    let answer = json(&trekr(&db, &gems, &["--def", &spec, "--json"]));
    assert_eq!(
        answer["status"], "resolved",
        "the sibling gem's method is reachable: {answer}"
    );
    assert_eq!(
        answer["context"].as_str(),
        app.canonicalize().unwrap().to_str(),
        "{answer}"
    );

    let out = trekr(&db, &app, &["--index", web.to_str().unwrap()]);
    assert_eq!(
        out.status.code(),
        Some(66),
        "a git gem is refreshed through its app, like any gem: {out:?}"
    );

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
}

/// A gem's opt-in core extensions are left out, as the stdlib's copy of them
/// is: the json gem's `json/add/` gives every object `to_json` only in an app
/// that requires it, and bundling json showed it to every app.
#[test]
fn a_bundled_json_gems_opt_in_extensions_are_left_out() {
    let (app, db) = scratch("json-add-app");
    let (gems, _) = scratch("json-add-gems");
    let lib = gems.join("gems/json-9.9.9/lib");
    fs::create_dir_all(lib.join("json/add")).unwrap();
    fs::write(
        lib.join("json.rb"),
        "module JSON\n  def self.generate(obj)\n  end\nend\n",
    )
    .unwrap();
    fs::write(
        lib.join("json/add/widget.rb"),
        "class Widget\n  def to_json(*)\n  end\nend\n",
    )
    .unwrap();
    git(&app, &["init", "-q"]);
    fs::write(
        app.join("app.rb"),
        "class Widget\nend\nWidget.new.to_json\nJSON.generate(1)\n",
    )
    .unwrap();
    fs::write(
        app.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    json (9.9.9)\n\
         \nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  json\n",
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
    let env = [("GEM_HOME", gems.to_str().unwrap())];
    let index = json(&trekr_env(&db, &app, &["--index", "--json"], &env));
    assert_eq!(index["gems"]["files"], 1, "{index}");

    let generate = json(&trekr(&db, &app, &["--def", "app.rb:4:6", "--json"]));
    assert_eq!(
        generate["status"], "resolved",
        "the gem is indexed: {generate}"
    );
    let to_json = json(&trekr(&db, &app, &["--def", "app.rb:3:12", "--json"]));
    assert_ne!(to_json["status"], "resolved", "{to_json}");

    let _ = fs::remove_dir_all(&app);
    let _ = fs::remove_dir_all(&gems);
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
fn with_no_index_an_unindexed_checkout_is_a_setup_problem_not_a_residue() {
    let (dir, db) = scratch("not-indexed");
    repo(&dir);

    for args in [
        vec!["--def", "widget.rb:7:5", "--no-index"],
        vec!["--refs", "Widget#helper", "--no-index"],
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

    // The environment says it for every command a script runs; `0` is no.
    let indexing = trekr_env(&db, &dir, &["--status"], &[("TREKR_NO_INDEX", "0")]);
    assert_eq!(indexing.status.code(), Some(2), "a flag that parses");
    let json = trekr_env(
        &db,
        &dir,
        &["--def", "widget.rb:7:5", "--json"],
        &[("TREKR_NO_INDEX", "1")],
    );
    let value: serde_json::Value = serde_json::from_slice(&json.stdout).expect("json on stdout");
    assert_eq!(value["status"], "not_indexed");
    assert!(
        value["repo"]
            .as_str()
            .is_some_and(|r| r.ends_with("not-indexed"))
    );
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM checkout"),
        0,
        "nothing indexed"
    );
    let index = trekr_env(
        &db,
        &dir,
        &["--index", "--json"],
        &[("TREKR_NO_INDEX", "1")],
    );
    assert!(index.status.success(), "an index is still an index");
    trekr(&db, &dir, &["--drop"]);

    // And it stops being a setup problem the moment it is indexed.
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let out = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--no-index"]);
    assert_eq!(out.status.code(), Some(0), "an indexed checkout answers");
}

/// Wait until no index of `dir` is under way: a position query leaves the
/// rest of a first index to a child that outlives it.
fn settled(db: &Path, dir: &Path) {
    for _ in 0..400 {
        let out = trekr(db, dir, &["--status", "--json"]);
        if out.status.code() == Some(0) && json(&out)["checkouts"][0].get("warming").is_none() {
            // The child prepares the tree after its last commit.
            std::thread::sleep(std::time::Duration::from_millis(200));
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("the index of {} never finished", dir.display());
}

/// A query in a checkout nobody indexed indexes it, and answers: no
/// `not_indexed`, and no setup step first (DEC-500).
#[test]
fn a_first_query_indexes_the_checkout_and_answers() {
    for (label, args) in [
        ("auto-refs", vec!["--refs", "Widget#helper"]),
        ("auto-refs-at", vec!["--refs", "widget.rb:6:7"]),
        ("auto-def", vec!["--def", "widget.rb:7:5"]),
        ("auto-bare", vec!["widget.rb:7:5"]),
        ("auto-card", vec!["Widget#resize"]),
        ("auto-const", vec!["Widget"]),
        ("auto-ancestors", vec!["--ancestors", "Widget"]),
        ("auto-dead", vec!["--dead", "."]),
    ] {
        let (dir, db) = scratch(label);
        repo(&dir);
        let mut asked = args.clone();
        asked.push("--json");
        let out = trekr(&db, &dir, &asked);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            matches!(out.status.code(), Some(0 | 1)),
            "{args:?}: {stderr}"
        );
        let answer = json(&out);
        assert_ne!(answer["status"], "not_indexed", "{args:?}: {answer}");
        assert_ne!(answer["status"], "warming", "{args:?}: {answer}");
        settled(&db, &dir);
        // The store it leaves is the one an index leaves: a reindex finds
        // nothing to parse.
        let again = json(&trekr(&db, &dir, &["--index", "--json"]));
        assert_eq!(again["indexed"]["parsed"], 0, "{args:?}: {again}");
        let _ = fs::remove_dir_all(&dir);
    }
}

/// A question about the whole checkout waits for the whole index — a
/// caller, a subclass, an unused method may be in any file — so its answer
/// is never partial. A position answers once its file and what it names are
/// in, and says if the rest is not.
#[test]
fn a_first_whole_checkout_question_waits_for_the_whole_index() {
    let (dir, db) = scratch("auto-whole");
    collision_repo(&dir);
    let out = trekr(&db, &dir, &["--refs", "Widget#save", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let answer = json(&out);
    assert!(answer.get("warming").is_none(), "{answer}");
    assert_eq!(answer["counts"]["excluded"], 2, "{answer}");
    let _ = fs::remove_dir_all(&dir);
}

/// `--status` reports; it never indexes.
#[test]
fn status_stays_read_only_in_an_unindexed_checkout() {
    let (dir, db) = scratch("auto-status");
    repo(&dir);
    let out = trekr(&db, &dir, &["--status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["status"], "not_indexed");
    assert_eq!(count(&db, "SELECT COUNT(*) FROM checkout"), 0);
    let _ = fs::remove_dir_all(&dir);
}

/// An index that takes longer than a second says so once on stderr — a
/// person or an agent waiting on it learns why — and stdout is still the
/// answer alone.
#[test]
fn a_first_index_longer_than_a_second_says_so_on_stderr() {
    let (dir, db) = scratch("auto-notice");
    repo(&dir);
    let (other, _) = scratch("auto-notice-other");
    repo(&other);
    assert!(trekr(&db, &other, &["--index"]).status.success());

    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let query = spawn_trekr(&db, &dir, &["--refs", "Widget#helper", "--json"]);
    std::thread::sleep(std::time::Duration::from_millis(1600));
    holder.execute_batch("ROLLBACK").unwrap();
    let out = query.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(
        stderr.matches("for the first time").count(),
        1,
        "one notice: {stderr}"
    );
    assert!(
        !stderr.contains('\r'),
        "no progress line off a terminal: {stderr}"
    );
    assert!(json(&out)["references"].is_array(), "stdout is the answer");

    // A quick one says nothing.
    let (quick, _) = scratch("auto-notice-quick");
    repo(&quick);
    let out = trekr(&db, &quick, &["--refs", "Widget#helper", "--json"]);
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    for dir in [dir, other, quick] {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Several first queries at once: one index, every query answered, no
/// "database is locked".
#[test]
fn first_queries_at_once_share_one_index() {
    let (dir, db) = scratch("auto-race");
    collision_repo(&dir);
    let asked = [
        vec!["--refs", "Widget#save", "--json"],
        vec!["--refs", "Widget#save", "--json"],
        vec!["--def", "app.rb:16:7", "--json"],
        vec!["--ancestors", "Widget", "--json"],
    ];
    let running: Vec<_> = asked
        .iter()
        .map(|args| spawn_trekr(&db, &dir, args))
        .collect();
    let outs: Vec<_> = running
        .into_iter()
        .map(|child| child.wait_with_output().unwrap())
        .collect();
    for (args, out) in asked.iter().zip(&outs) {
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("locked"), "{args:?}: {stderr}");
    }
    assert_eq!(json(&outs[0]), json(&outs[1]));
    assert_eq!(json(&outs[0])["counts"]["excluded"], 2);
    settled(&db, &dir);
    // One of them started the index; the rest found its claim and waited.
    let rows = json(&trekr(&db, &dir, &["--usage", "--json"]));
    let spawned: i64 = rows
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["flags"].as_str().unwrap_or("").contains("indexed"))
        .map(|r| r["count"].as_i64().unwrap())
        .sum();
    assert_eq!(spawned, 1, "{rows}");
    let _ = fs::remove_dir_all(&dir);
}

/// A first index another process is running — the language server's, or a
/// `trekr --index` — is waited for, not run again; and one whose process
/// died is finished by the query that finds it.
#[test]
fn a_query_waits_for_an_index_under_way_and_finishes_one_cut_short() {
    let (dir, db) = scratch("auto-behind");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let indexer = stand_in_indexer(&db, &dir);
    let started = std::time::Instant::now();
    let out = trekr(&db, &dir, &["--refs", "Widget#helper", "--json"]);
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(1000),
        "waited for the index under way"
    );
    indexer.join();
    assert_eq!(out.status.code(), Some(0));
    assert!(json(&out).get("warming").is_none(), "and finished it");

    // A position answers from the part read when it finds something, and
    // says so; a miss there is not final, so it waits for the rest.
    let indexer = stand_in_indexer(&db, &dir);
    let started = std::time::Instant::now();
    let hit = trekr(&db, &dir, &["--def", "widget.rb:7:5", "--json"]);
    assert!(indexer.running(), "answered without waiting for the index");
    assert_eq!(hit.status.code(), Some(0));
    assert!(json(&hit)["warming"].is_object(), "{}", stdout(&hit));
    let miss = trekr(&db, &dir, &["--def", "widget.rb:1:17", "--json"]);
    assert!(started.elapsed() >= std::time::Duration::from_millis(1000));
    indexer.join();
    assert_eq!(miss.status.code(), Some(1), "a miss from the whole index");
    assert!(json(&miss).get("warming").is_none(), "{}", stdout(&miss));

    // `--index` waits its turn the same way, and says so.
    let indexer = stand_in_indexer(&db, &dir);
    let out = trekr(&db, &dir, &["--index", "--json"]);
    indexer.join();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{stderr}");
    assert!(stderr.contains("already indexing"), "{stderr}");
    assert!(json(&out)["indexed"].is_object());
    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert!(status["checkouts"][0].get("warming").is_none(), "{status}");
    let _ = fs::remove_dir_all(&dir);
}

/// A position answered from the part of a first index says, at a terminal
/// or not, that the rest is still being read in the background; and when a
/// miss there waits for the rest, it waits for its own index as its own.
#[test]
fn an_early_answer_says_the_rest_is_indexed_in_the_background() {
    let (dir, db) = scratch("auto-background");
    repo(&dir);
    let stall = [("TREKR_TEST_STALL_MS", "4000")];
    let limit = std::time::Duration::from_secs(20);
    let hit = trekr_within(&db, &dir, &["--def", "widget.rb:7:5"], &stall, limit);
    let stderr = String::from_utf8_lossy(&hit.stderr);
    assert_eq!(hit.status.code(), Some(0), "{stderr}");
    assert!(stderr.contains("still being indexed"), "{stderr}");
    assert!(stderr.contains("in the background"), "{stderr}");
    settled(&db, &dir);

    let (dir, db) = scratch("auto-background-miss");
    repo(&dir);
    let miss = trekr_within(&db, &dir, &["--def", "widget.rb:1:17"], &stall, limit);
    let stderr = String::from_utf8_lossy(&miss.stderr);
    assert_eq!(miss.status.code(), Some(1), "{stderr}");
    assert!(!stderr.contains("another trekr"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

/// An empty store's `--status --all` says how a checkout gets indexed now:
/// by its first query.
#[test]
fn status_of_an_empty_store_says_the_first_query_indexes() {
    let (dir, db) = scratch("status-empty");
    repo(&dir);
    let out = trekr(&db, &dir, &["--status", "--all"]);
    let text = stdout(&out);
    assert!(
        text.contains("the first query in a checkout indexes it"),
        "{text}"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A repo with a second file that names `Widget`, and its first index
/// started by hand — no file asked first — pausing after its first part and
/// again as the rest's write holds the store.
fn slow_first_index(label: &str, bulk_ms: &str) -> (PathBuf, PathBuf, std::process::Child) {
    let (dir, db) = scratch(label);
    repo(&dir);
    fs::write(
        dir.join("other.rb"),
        "class Other\n  def go\n    Widget.new\n    Nowhere.new\n  end\nend\n",
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
            "other",
        ],
    );
    let winner = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_TEST_STALL_MS", "3000")
        .env("TREKR_TEST_STALL_BULK_MS", bulk_ms)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    (dir, db, winner)
}

/// A query whose file another process's first index has not read hands it
/// to that index, which reads it next — in its next part (DEC-512).
#[test]
fn a_query_behind_anothers_first_index_has_its_file_read_in_the_next_part() {
    let limit = std::time::Duration::from_secs(30);
    let ask = ["--def", "other.rb:3:5", "--json"];
    // Asked while the first part is in.
    let (dir, db, mut winner) = slow_first_index("handed-part", "6000");
    std::thread::sleep(std::time::Duration::from_millis(1000));
    let started = std::time::Instant::now();
    let out = trekr_within(&db, &dir, &ask, &[], limit);
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(json(&out)["warming"].is_object(), "{}", stdout(&out));
    assert!(
        took < std::time::Duration::from_millis(4500),
        "{took:?}: {stderr}"
    );
    winner.wait().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

/// …or, while the rest's write holds the store, into its early store, which
/// the query then answers from — and a miss there asks the whole once it is
/// in (DEC-512).
#[test]
fn a_query_behind_anothers_first_index_is_answered_from_its_early_store() {
    let limit = std::time::Duration::from_secs(30);
    let ask = ["--def", "other.rb:3:5", "--json"];
    let (dir, db, mut winner) = slow_first_index("handed-early", "6000");
    std::thread::sleep(std::time::Duration::from_millis(4000));
    let started = std::time::Instant::now();
    let out = trekr_within(&db, &dir, &ask, &[], limit);
    let took = started.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert_eq!(json(&out)["definition"][0]["path"], "widget.rb");
    assert!(
        took < std::time::Duration::from_millis(3000),
        "{took:?}: {stderr}"
    );
    let miss = trekr_within(&db, &dir, &["--def", "other.rb:4:5", "--json"], &[], limit);
    let stderr = String::from_utf8_lossy(&miss.stderr);
    assert_eq!(miss.status.code(), Some(1), "{stderr}");
    let answer = json(&miss);
    assert!(answer.get("warming").is_none(), "{answer}");
    winner.wait().unwrap();
    let _ = fs::remove_dir_all(&dir);
}

/// A miss from another's early store waits for the rest as a query waits
/// for any index — saying so past a second, bounded by the writer wait —
/// rather than asking again in a fresh process that finds the same early
/// store and misses again, for as long as that index stands still.
#[test]
fn a_miss_from_an_early_store_waits_a_bounded_while_and_says_so() {
    /// Stopped however the test ends: it would stall a minute more.
    struct Reaped(std::process::Child);
    impl Drop for Reaped {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (dir, db, winner) = slow_first_index("early-stuck", "60000");
    let winner = Reaped(winner);
    std::thread::sleep(std::time::Duration::from_millis(4000));
    let wait = [("TREKR_TEST_WRITER_WAIT_MS", "2500")];
    let limit = std::time::Duration::from_secs(20);
    let miss = trekr_within(
        &db,
        &dir,
        &["--def", "other.rb:4:5", "--json"],
        &wait,
        limit,
    );
    drop(winner);
    let stderr = String::from_utf8_lossy(&miss.stderr);
    assert_eq!(miss.status.code(), Some(2), "{stderr}");
    assert_eq!(json(&miss)["status"], "incomplete", "{}", stdout(&miss));
    assert!(stderr.contains("waiting for the index"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

/// A first index that loses the checkout to another hands what it was told
/// to read first — the editor's open files — to the winner (DEC-512).
#[test]
fn a_losing_index_hands_its_hints_to_the_winner() {
    let (dir, db) = scratch("handed-loser");
    repo(&dir);
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(2));
    let mut other = Command::new("sleep").arg("30").spawn().unwrap();
    mark_warming(&db, &dir, other.id(), 0, 1);
    let mut loser = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_BACKGROUND", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let open = fs::canonicalize(&dir).unwrap().join("widget.rb");
    {
        use std::io::Write;
        let mut stdin = loser.stdin.take().unwrap();
        writeln!(stdin, "{}", open.display()).unwrap();
        // Kept open, as the language server keeps it.
        std::mem::forget(stdin);
    }
    let file = PathBuf::from(format!("{}.hints-{}", db.display(), other.id()));
    let started = std::time::Instant::now();
    let mut said = String::new();
    while started.elapsed() < std::time::Duration::from_secs(10) {
        said = fs::read_to_string(&file).unwrap_or_default();
        if !said.is_empty() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    other.kill().unwrap();
    other.wait().unwrap();
    let _ = loser.kill();
    loser.wait().unwrap();
    assert_eq!(said.trim(), open.display().to_string());
    let _ = fs::remove_dir_all(&dir);
}

/// A hand-run `--index` that outwaits another's index of the checkout says
/// that is what it waited for — not a write lock, which it never waited on.
#[test]
fn an_index_that_outwaits_another_index_says_so() {
    let (dir, db) = scratch("claim-outwaited");
    repo(&dir);
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(2));
    let mut other = Command::new("sleep").arg("30").spawn().unwrap();
    mark_warming(&db, &dir, other.id(), 0, 1);
    let out = trekr_env(
        &db,
        &dir,
        &["--index"],
        &[("TREKR_TEST_WRITER_WAIT_MS", "1500")],
    );
    other.kill().unwrap();
    other.wait().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("index of it outlasted"), "{stderr}");
    assert!(!stderr.contains("write lock"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

/// A first query's index leaves no write-ahead log behind, as `--index`
/// does not: the query's own connection keeps the child from being the
/// last to close, which is what would have checkpointed and removed it.
#[test]
fn a_first_query_leaves_no_write_ahead_log() {
    let (dir, db) = scratch("auto-wal");
    repo(&dir);
    let out = trekr(&db, &dir, &["--refs", "Widget#helper", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let wal = PathBuf::from(format!("{}-wal", db.display()));
    // A page or two a reader's closing `optimize` writes may follow; the
    // index's own frames (hundreds of KB here, tens of MB on a real app) not.
    let size = fs::metadata(&wal).map_or(0, |m| m.len());
    assert!(size < 64 * 1024, "{} left at {size} bytes", wal.display());
    let _ = fs::remove_dir_all(&dir);
}

/// A position's answer that no index can change — nothing under the cursor
/// — is final while an index runs, as after it: exit 1, no `warming`, no
/// wait. One the index can change — a namespace resolved with nowhere yet
/// to go — waits for the rest.
#[test]
fn a_position_the_index_cannot_change_answers_at_once_while_it_runs() {
    let (dir, db) = scratch("auto-final");
    repo(&dir);
    fs::write(dir.join("thing.rb"), "class Outer::Thing\nend\n").unwrap();
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
            "thing",
        ],
    );
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    let indexer = stand_in_indexer(&db, &dir);
    let started = std::time::Instant::now();
    let blank = trekr(&db, &dir, &["--def", "widget.rb:3:1", "--json"]);
    assert!(indexer.running(), "answered without waiting for the index");
    assert_eq!(blank.status.code(), Some(1), "{}", stdout(&blank));
    assert!(json(&blank).get("warming").is_none(), "{}", stdout(&blank));
    let namespace = trekr(&db, &dir, &["--def", "thing.rb:1:7", "--json"]);
    assert!(started.elapsed() >= std::time::Duration::from_millis(1000));
    indexer.join();
    assert!(
        json(&namespace).get("warming").is_none(),
        "{}",
        stdout(&namespace)
    );
    let _ = fs::remove_dir_all(&dir);
}

/// An answer from the file alone — nothing under the cursor, a symbol, a
/// local, the definition the cursor is on — waits for no part of an index
/// that has not read the file yet, and claims what the file says: confidence
/// whole, no `warming`.
#[test]
fn an_answer_from_the_file_alone_waits_for_no_index() {
    let (dir, db) = scratch("auto-file-alone");
    repo(&dir);
    assert_eq!(trekr(&db, &dir, &["--status"]).status.code(), Some(2));
    let indexer = stand_in_indexer(&db, &dir);
    for (at, code) in [
        ("widget.rb:3:1", 1),
        ("widget.rb:4:16", 0),
        ("widget.rb:6:14", 0),
        ("widget.rb:6:7", 0),
    ] {
        let out = trekr(&db, &dir, &["--def", at, "--json"]);
        let answer = json(&out);
        assert_eq!(out.status.code(), Some(code), "{at}: {answer}");
        assert!(answer.get("warming").is_none(), "{at}: {answer}");
        if code == 0 {
            assert_eq!(answer["confidence"], 1.0, "{at}: {answer}");
        }
    }
    assert!(indexer.running(), "answered without waiting for the index");
    indexer.join();
    let _ = fs::remove_dir_all(&dir);
}

/// A process that marks `dir` as an index filling it, lives a second and a
/// half, and is reaped the moment it ends: a zombie still answers a liveness
/// check, and would read as an index under way for good.
fn stand_in_indexer(db: &Path, dir: &Path) -> StandIn {
    let mut indexer = Command::new("sleep").arg("1.5").spawn().unwrap();
    mark_warming(db, dir, indexer.id(), 1, 4);
    let ended = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let reaper = std::thread::spawn({
        let ended = ended.clone();
        move || {
            indexer.wait().unwrap();
            ended.store(true, std::sync::atomic::Ordering::SeqCst);
        }
    });
    StandIn { ended, reaper }
}

struct StandIn {
    ended: std::sync::Arc<std::sync::atomic::AtomicBool>,
    reaper: std::thread::JoinHandle<()>,
}

impl StandIn {
    /// Still holding its mark: what an answer that did not wait for the
    /// index came back during, however slow the machine.
    fn running(&self) -> bool {
        !self.ended.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn join(self) {
        self.reaper.join().unwrap();
    }
}

/// A first index a query starts that cannot get the write lock in time is
/// reported as `--index` reports it (DEC-400): incomplete, exit 2.
#[test]
fn a_first_query_whose_index_outwaits_the_lock_is_incomplete() {
    let (dir, db) = scratch("auto-outwaited");
    repo(&dir);
    let (other, _) = scratch("auto-outwaited-other");
    repo(&other);
    assert!(trekr(&db, &other, &["--index"]).status.success());
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = trekr_env(
        &db,
        &dir,
        &["--refs", "Widget#helper", "--json"],
        &[("TREKR_TEST_WRITER_WAIT_MS", "500")],
    );
    holder.execute_batch("ROLLBACK").unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert_eq!(json(&out)["status"], "incomplete", "{stderr}");
    assert!(stderr.contains("trekr --index"), "{stderr}");
    for dir in [dir, other] {
        let _ = fs::remove_dir_all(&dir);
    }
}

/// Run trekr with extra environment, failing the test rather than hanging
/// it when trekr outlives `limit`.
fn trekr_within(
    db: &Path,
    cwd: &Path,
    args: &[&str],
    vars: &[(&str, &str)],
    limit: std::time::Duration,
) -> Output {
    let mut command = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")));
    command
        .args(args)
        .current_dir(cwd)
        .env("TREKR_DB", db)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in vars {
        command.env(key, value);
    }
    let mut child = command.spawn().expect("spawn trekr");
    let started = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if started.elapsed() > limit {
            let _ = child.kill();
            let out = child.wait_with_output().unwrap();
            panic!(
                "trekr {args:?} still running after {limit:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

/// A first query's index that dies once it has claimed the checkout —
/// killed, or failing to write — ends the query with what killed it, and
/// the next query takes the dead claim over rather than waiting on it.
#[test]
fn a_first_query_whose_index_dies_after_claiming_ends() {
    let limit = std::time::Duration::from_secs(20);
    for (hook, code, says) in [
        ("kill", 2, "stopped by a signal"),
        ("fail", 74, "disk I/O error"),
    ] {
        let (dir, db) = scratch(&format!("auto-dies-{hook}"));
        repo(&dir);
        let out = trekr_within(
            &db,
            &dir,
            &["--refs", "Widget#helper", "--json"],
            &[("TREKR_TEST_AFTER_CLAIM", hook)],
            limit,
        );
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(code), "{hook}: {stderr}");
        assert!(stderr.contains(says), "{hook}: {stderr}");

        let started = std::time::Instant::now();
        let next = trekr_within(
            &db,
            &dir,
            &["--refs", "Widget#helper", "--json"],
            &[],
            limit,
        );
        let stderr = String::from_utf8_lossy(&next.stderr);
        assert_eq!(next.status.code(), Some(0), "{hook}: {stderr}");
        assert!(json(&next).get("warming").is_none(), "{hook}: {stderr}");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{hook}: the dead claim is taken over, not waited out"
        );
        let _ = fs::remove_dir_all(&dir);
    }
}

/// A mark whose pid now names another process — the pid reused since its
/// writer died — is no index under way: a hand-run `--index` and a query
/// each take it over at once rather than waiting the writer wait out.
#[test]
fn a_claim_whose_pid_was_reused_is_taken_over() {
    let limit = std::time::Duration::from_secs(20);
    let wait = [("TREKR_TEST_WRITER_WAIT_MS", "3000")];
    for args in [
        vec!["--index", "--json"],
        vec!["--refs", "Widget#helper", "--json"],
    ] {
        let (dir, db) = scratch(&format!("reused-{}", args[0].trim_start_matches('-')));
        repo(&dir);
        assert!(trekr(&db, &dir, &["--status"]).status.code() == Some(2));
        let root = fs::canonicalize(&dir).unwrap();
        // This test's own pid: running, but not since the start the mark names.
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
                [
                    format!("warming {}", root.to_string_lossy()),
                    format!("{} 0 1 own start:1", std::process::id()),
                ],
            )
            .unwrap();
        let out = trekr_within(&db, &dir, &args, &wait, limit);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(0), "{args:?}: {stderr}");
        assert!(!stderr.contains("already indexing"), "{args:?}: {stderr}");
        assert!(!stderr.contains("another trekr"), "{args:?}: {stderr}");
        let _ = fs::remove_dir_all(&dir);
    }
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
        // Under the store's 5 s busy timeout, the least a wait on its lock
        // costs: a loaded machine can take over a second just to run trekr.
        assert!(
            elapsed < std::time::Duration::from_secs(4),
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

/// A file trekr finds by its name rather than by `git ls-files` — the
/// lockfile, the routes, the app's config — is read as bounded as a source:
/// one that is a link to `/dev/zero` is passed over, not read forever.
#[cfg(unix)]
#[test]
fn a_file_found_by_its_name_is_read_bounded() {
    let (dir, db) = scratch("named-devices");
    repo(&dir);
    fs::create_dir_all(dir.join("config")).unwrap();
    for name in [
        "Gemfile",
        "Gemfile.lock",
        "config/routes.rb",
        "config/application.rb",
    ] {
        std::os::unix::fs::symlink("/dev/zero", dir.join(name)).unwrap();
    }
    let limit = std::time::Duration::from_secs(30);
    for args in [&["--index", "--json"][..], &["--dead", ".", "--json"]] {
        let out = trekr_within(&db, &dir, args, &[], limit);
        assert!(out.status.code().is_some(), "{args:?}: {out:?}");
        assert!(json(&out).is_object(), "{args:?}: {}", stdout(&out));
    }
    let _ = fs::remove_dir_all(&dir);
}

/// A path named as a source is read only if it is a regular file of bounded
/// size, by every command — and a checkout holding a link to a device still
/// indexes, leaving it out.
#[cfg(unix)]
#[test]
fn a_pipe_or_a_device_is_refused_rather_than_read() {
    let (dir, db) = scratch("devices");
    repo(&dir);
    std::os::unix::fs::symlink("/dev/zero", dir.join("zero.rb")).unwrap();
    let made = Command::new("mkfifo")
        .arg(dir.join("pipe.rb"))
        .status()
        .unwrap();
    assert!(made.success());
    let limit = std::time::Duration::from_secs(30);

    let indexed = trekr_within(&db, &dir, &["--index", "--json"], &[], limit);
    assert!(indexed.status.success(), "{indexed:?}");
    assert_eq!(json(&indexed)["indexed"]["files"], 1, "only widget.rb");

    for path in ["pipe.rb", "zero.rb"] {
        for command in ["--refs", "--def"] {
            let at = format!("{path}:1:1");
            let out = trekr_within(&db, &dir, &[command, &at], &[], limit);
            assert_eq!(out.status.code(), Some(64), "{command} {at}: {out:?}");
        }
    }

    // --dead reads its scope and every tracked path: neither may block.
    git(&dir, &["add", "zero.rb"]);
    for scope in [".", "widget.rb"] {
        let out = trekr_within(&db, &dir, &["--dead", scope, "--json"], &[], limit);
        assert!(out.status.code().is_some(), "--dead {scope}: {out:?}");
        let dead = json(&out);
        let rows = dead["candidates"].as_array().unwrap();
        for row in rows {
            let mentions = &row["mentions_by_name"];
            assert!(mentions.is_null() || mentions.is_u64(), "a count: {row}");
        }
        assert!(
            rows.iter().any(|row| row["mentions_by_name"].is_u64()),
            "{dead}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

/// A column past the end of its line is the line's end, as the editor
/// clamps one: never a variable on a later line.
#[test]
fn a_column_past_the_line_stays_on_its_line() {
    let (dir, db) = scratch("past-eol");
    repo(&dir);
    fs::write(
        dir.join("counter.rb"),
        "class Counter\n  def initialize(name)\n    count = 1\n    puts count\n  end\nend\n",
    )
    .unwrap();
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    for command in ["--refs", "--def"] {
        let out = trekr(&db, &dir, &[command, "counter.rb:2:99", "--json"]);
        let said = stdout(&out);
        assert!(!said.contains("\"count\""), "{command}: {said}");
    }
    // At the end of a line holding one, it is that variable, as in the editor.
    let out = trekr(&db, &dir, &["--refs", "counter.rb:4:99", "--json"]);
    assert_eq!(json(&out)["name"], "count", "{out:?}");
    let _ = fs::remove_dir_all(&dir);
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
    // The class nothing names is a candidate too, after the methods.
    assert!(
        text.contains("7 candidates in 1 file(s): 4 unreferenced, 1 convention-only, 2 single-caller (5 clear, 2 lower); 1 of them classes, modules or constants"),
        "{text}"
    );
    assert!(text.contains("  class Thing  "), "{text}");
    assert_eq!(by_name["Thing"]["kind"], "class", "{value}");
    assert_eq!(by_name["never_used"]["kind"], "method", "{value}");
    assert_eq!(value["summary"]["kinds"]["class"], 1, "{value}");
    assert_eq!(value["summary"]["candidates"], 7, "{value}");
    assert_eq!(value["summary"]["tiers"]["single-caller"], 2, "{value}");
    assert_eq!(value["summary"]["tiers"]["override"], 0, "{value}");
    assert_eq!(value["summary"]["confidence"]["lower"], 2, "{value}");
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

/// Every index prepares the tree snapshot the next query would otherwise
/// assemble, the one the LSP starts in the background and one someone runs.
#[test]
fn an_index_prepares_the_tree() {
    let (dir, db) = scratch("prebuild");
    repo(&dir);
    let trees = db.with_extension("trees");
    let files = || fs::read_dir(&trees).map_or(0, |d| d.count());
    trekr(&db, &dir, &["--index"]);
    assert_eq!(files(), 1, "a foreground index prepares it too");

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
    let flagged = trekr(&db, &dir, &["--ancestors", "Gadget", "--profile"]);
    let profile = String::from_utf8_lossy(&flagged.stderr);
    assert!(
        profile.contains("snapshot-load"),
        "--profile too: {profile}"
    );
    let off = trekr_env(
        &db,
        &dir,
        &["--ancestors", "Gadget"],
        &[("TREKR_PROFILE", "0")],
    );
    let profile = String::from_utf8_lossy(&off.stderr);
    assert!(!profile.contains("snapshot-load"), "0 is off: {profile}");
    let _ = fs::remove_dir_all(&dir);
}

/// The snapshot holds the namespace, not methods: an edit that adds and moves
/// methods keeps it, and the query after still finds the new method (DEC-194).
#[test]
fn a_method_edit_keeps_the_tree_snapshot() {
    let (dir, db) = scratch("method-edit");
    repo(&dir);
    let trees = db.with_extension("trees");
    let names = || -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&trees)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };
    trekr(&db, &dir, &["--index"]);
    let before = names();
    assert_eq!(before.len(), 1);

    fs::write(
        dir.join("widget.rb"),
        "class Widget < Base\n  include Trackable\n\n  attr_reader :name\n\n  \
         def resize(width, height = 1)\n    helper\n    polish\n  end\n\n  \
         def polish\n  end\n\n  private\n\n  def helper\n  end\nend\n",
    )
    .unwrap();
    trekr(&db, &dir, &["--index"]);
    assert_eq!(names(), before, "the same snapshot, not a rebuilt one");
    let refs = json(&trekr(&db, &dir, &["--refs", "Widget#polish", "--json"]));
    assert_eq!(refs["counts"]["confirmed"], 1, "{refs}");

    // A new class is the namespace's, and moves it.
    fs::write(dir.join("gadget.rb"), "class Gadget\nend\n").unwrap();
    trekr(&db, &dir, &["--index"]);
    assert_ne!(names(), before);
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Gadget"]).status.code(),
        Some(0)
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
    assert_eq!(files(), 1, "the index wrote its snapshot");

    // A snapshot under a key no checkout's index names any more: what a
    // store move that no index or query followed leaves behind.
    let current = fs::read_dir(&trees)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let name = current.file_name().unwrap().to_str().unwrap();
    let (tag, _) = name.split_once('-').unwrap();
    fs::copy(
        &current,
        trees.join(format!("{tag}-{}.tree", "0".repeat(40))),
    )
    .unwrap();
    assert_eq!(files(), 2);
    let dry = trekr(&db, &dir, &["--gc", "--dry-run", "--json"]);
    assert_eq!(
        dry.status.code(),
        Some(0),
        "stale snapshots are something to collect"
    );
    let dry = json(&dry);
    assert_eq!(dry["snapshots"]["files"], 1, "{dry}");
    assert!(dry["snapshots"]["bytes"].as_u64().unwrap() > 0);
    assert_eq!(files(), 2, "a dry run removes nothing");

    let text = stdout(&trekr(&db, &dir, &["--gc"]));
    assert!(text.contains("collected 1 tree snapshots"), "{text}");
    assert_eq!(files(), 1, "the current snapshot stays");
    let again = trekr(&db, &dir, &["--gc", "--json"]);
    assert_eq!(again.status.code(), Some(1), "nothing left to collect");
    assert_eq!(json(&again)["snapshots"]["files"], 0);
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Widget"]).status.code(),
        Some(0),
        "and still answers"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// What an older trekr, a recovery or a dead index left beside the store is
/// listed by `--status`; `--gc` removes a set-aside copy and a dead index's
/// early store at once, and another trekr's store once it has been idle as
/// long as `--older-than`. A running index's early store is not kept: it is
/// in use.
#[test]
fn status_lists_and_gc_removes_what_is_kept_beside_the_store() {
    let (dir, db) = scratch("gc-kept");
    repo(&dir);
    trekr(&db, &dir, &["--index"]);
    let stem = db.file_stem().unwrap().to_str().unwrap();
    let side = db.with_file_name(format!("{stem}.v3.db"));
    let broken = db.with_file_name(format!(
        "{}.broken-100",
        db.file_name().unwrap().to_str().unwrap()
    ));
    fs::write(&side, b"old").unwrap();
    fs::write(&broken, b"bad").unwrap();
    let name = db.file_name().unwrap().to_str().unwrap();
    let early = |pid: u32| {
        let early = db.with_file_name(format!("{name}.early-{pid}"));
        fs::create_dir_all(&early).unwrap();
        fs::write(early.join(name), b"copy").unwrap();
        early
    };
    let dead = early(i32::MAX as u32);
    let running = early(std::process::id());

    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    let kinds: Vec<&str> = status["kept"]
        .as_array()
        .unwrap()
        .iter()
        .map(|k| k["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["side", "broken", "early"], "{status}");
    let text = stdout(&trekr(&db, &dir, &["--status"]));
    assert!(text.contains("store v3"), "{text}");
    assert!(text.contains("early store"), "{text}");

    let gc = trekr(&db, &dir, &["--gc", "--json"]);
    assert_eq!(gc.status.code(), Some(0));
    assert_eq!(json(&gc)["kept"].as_array().unwrap().len(), 2);
    assert!(!broken.exists(), "a set-aside copy goes at any age");
    assert!(!dead.exists(), "so does a dead index's early store");
    assert!(running.exists(), "a running index's is its own");
    assert!(side.exists(), "a recently used store stays");

    trekr(&db, &dir, &["--gc", "--older-than", "0"]);
    assert!(!side.exists());
    assert_eq!(
        trekr(&db, &dir, &["--ancestors", "Widget"]).status.code(),
        Some(0),
        "and the store in use still answers"
    );
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
/// A spawned trekr's stderr, read as it is written: a test acts once the
/// process has said where it is, not after a guess at how long that takes.
struct Said {
    text: std::sync::Arc<std::sync::Mutex<String>>,
    reader: std::thread::JoinHandle<()>,
}

impl Said {
    fn of(child: &mut std::process::Child) -> Said {
        use std::io::Read;
        let mut pipe = child.stderr.take().expect("stderr piped");
        let text = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let reader = std::thread::spawn({
            let text = text.clone();
            move || {
                let mut buffer = [0u8; 4096];
                while let Ok(n @ 1..) = pipe.read(&mut buffer) {
                    let more = String::from_utf8_lossy(&buffer[..n]);
                    text.lock().unwrap().push_str(&more);
                }
            }
        });
        Said { text, reader }
    }

    fn wait_for(&self, what: &str) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        while !self.text.lock().unwrap().contains(what) {
            assert!(
                std::time::Instant::now() < deadline,
                "never said {what:?}: {}",
                self.text.lock().unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    /// Everything it said, once it has closed stderr.
    fn all(self) -> String {
        self.reader.join().unwrap();
        std::mem::take(&mut *self.text.lock().unwrap())
    }
}

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
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    rusqlite::Connection::open(db)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode=WAL; CREATE TABLE checkout (x); PRAGMA user_version = 1;",
        )
        .unwrap();
}

/// An upgrade drops every Ruby's signatures, so their core files go with
/// them, and so does what a build before a core directory per store wrote
/// beside it — but only a `core/` trekr wrote (DEC-274).
#[test]
fn an_upgrade_removes_the_core_files_earlier_builds_left() {
    let (dir, _) = scratch("core-upgrade");
    repo(&dir);
    let (store, _) = scratch("core-upgrade-store");
    let db = store.join("trekr.db");
    let rspec = include_str!("../src/tree/rspec.rb");
    let legacy = store.join("core");
    for (path, text) in [
        ("RSpec.rb", rspec),
        ("String.rb", "class String\nend\n"),
        ("stdlib/Pathname.rb", "class Pathname\nend\n"),
        ("rbs-9.9.9-deadbeef/String.rb", "class String\nend\n"),
    ] {
        fs::create_dir_all(legacy.join(path).parent().unwrap()).unwrap();
        fs::write(legacy.join(path), text).unwrap();
    }
    let stale = store.join("trekr.core/rbs-9.9.9-0badcafe/String.rb");
    fs::create_dir_all(stale.parent().unwrap()).unwrap();
    fs::write(&stale, "class String\nend\n").unwrap();
    // Someone else's `core/`, beside a store of its own: never trekr's.
    let (other, _) = scratch("core-upgrade-other");
    let theirs = other.join("core/String.rb");
    fs::create_dir_all(theirs.parent().unwrap()).unwrap();
    fs::write(&theirs, "class String\nend\n").unwrap();

    for db in [&db, &other.join("trekr.db")] {
        old_store(db);
        assert!(trekr(db, &dir, &["--status"]).status.code().is_some());
    }
    assert!(!legacy.exists(), "0.8.0's flat core directory is gone");
    assert!(!stale.exists(), "a Ruby's files the upgrade dropped");
    assert!(theirs.exists(), "a core/ trekr did not write stays");

    for dir in [&dir, &store, &other] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// `--gc` drops the signatures of a stdlib it collects, and each Ruby's core
/// files no signatures in the store are served from (DEC-274).
#[test]
fn gc_collects_a_rubys_signatures_and_core_files() {
    let (dir, db) = scratch("gc-core");
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    let (home, _) = scratch("gc-core-home");
    fake_ruby(&home, "9.8.7", &[], &[]);
    fake_ruby(&home, "9.7.1", &[], &[]);
    let env = [("HOME", home.to_str().unwrap())];
    let core_dirs = || {
        let mut names: Vec<String> = fs::read_dir(db.with_extension("core"))
            .map(|d| {
                d.flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| n.starts_with("rbs-"))
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    };
    for version in ["9.8.7", "9.7.1"] {
        fs::write(dir.join(".ruby-version"), format!("{version}\n")).unwrap();
        trekr_env(&db, &dir, &["--index"], &env);
        trekr_env(&db, &dir, &["--def", "use.rb:1:5"], &env);
    }
    assert_eq!(core_dirs().len(), 2, "one per Ruby read: {:?}", core_dirs());

    let dry = json(&trekr_env(
        &db,
        &dir,
        &["--gc", "--older-than", "0", "--dry-run", "--json"],
        &env,
    ));
    assert_eq!(dry["signatures"], 1, "{dry}");
    assert!(dry["core_files"]["files"].as_u64().unwrap() > 0, "{dry}");
    assert_eq!(core_dirs().len(), 2, "a dry run removes nothing");
    let done = json(&trekr_env(
        &db,
        &dir,
        &["--gc", "--older-than", "0", "--json"],
        &env,
    ));
    assert_eq!(done["signatures"], 1, "{done}");
    assert_eq!(core_dirs().len(), 1, "{done}");
    let upcase = json(&trekr_env(
        &db,
        &dir,
        &["--def", "use.rb:1:5", "--json"],
        &env,
    ));
    assert_eq!(upcase["owner"], "String", "the Ruby in use keeps its core");
    let again = trekr_env(&db, &dir, &["--gc", "--older-than", "0", "--json"], &env);
    assert_eq!(
        again.status.code(),
        Some(1),
        "nothing left: {}",
        stdout(&again)
    );

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
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
    let out = trekr(
        &db,
        &dir,
        &["--refs", "Widget#resize", "--json", "--no-index"],
    );
    assert_eq!(out.status.code(), Some(2));
    let answer = json(&out);
    assert_eq!(answer["status"], "not_indexed");
    let reason = answer["reason"].as_str().unwrap();
    assert!(reason.contains("v1"), "{reason}");
    // `--status` says why it is empty too, rather than looking never used.
    let out = trekr(&db, &dir, &["--status", "--json"]);
    assert_eq!(out.status.code(), Some(2));
    let reason = json(&out)["reason"].as_str().unwrap().to_string();
    assert!(reason.contains("v1"), "{reason}");

    // Once anything is indexed again, "not indexed" is about the checkout.
    let (other, _) = scratch("upgraded-other");
    repo(&other);
    assert!(trekr(&db, &other, &["--index"]).status.success());
    let answer = json(&trekr(
        &db,
        &dir,
        &["--refs", "Widget#resize", "--json", "--no-index"],
    ));
    let reason = answer["reason"].as_str().unwrap();
    assert!(!reason.contains("format changed"), "{reason}");

    // Without it, the query indexes what the upgrade dropped, and answers.
    let out = trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]);
    assert_eq!(out.status.code(), Some(1), "no callers, found by looking");
    assert_eq!(json(&out)["owner"], "Widget");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
}

/// `--status` answers for the checkout it is asked from, as a query does: one
/// nobody indexed is `not_indexed`, exit 2 — never another checkout's row.
#[test]
fn status_in_an_unindexed_checkout_says_so_rather_than_showing_another() {
    let (dir, db) = scratch("status-here");
    repo(&dir);
    let (other, _) = scratch("status-here-other");
    repo(&other);
    assert!(trekr(&db, &other, &["--index"]).status.success());

    let query = trekr(
        &db,
        &dir,
        &["--refs", "Widget#resize", "--json", "--no-index"],
    );
    let out = trekr(&db, &dir, &["--status", "--json"]);
    assert_eq!(out.status.code(), Some(2), "{}", stdout(&out));
    assert_eq!(out.status.code(), query.status.code());
    let answer = json(&out);
    assert_eq!(answer["status"], "not_indexed", "{answer}");
    assert_eq!(answer["repo"], json(&query)["repo"], "{answer}");
    assert!(answer["reason"].is_string(), "{answer}");
    assert!(answer["hint"].is_string(), "{answer}");
    assert_eq!(answer["checkouts"], serde_json::json!([]), "{answer}");
    assert_eq!(answer["others"]["repos"], 1, "the rest is summarized apart");

    let text = trekr(&db, &dir, &["--status"]);
    assert_eq!(text.status.code(), Some(2));
    let printed = stdout(&text);
    assert!(printed.contains("is not indexed"), "{printed}");
    assert!(printed.contains("1 other repo"), "{printed}");

    // `--context` asks about a checkout from anywhere.
    let other_arg = other.to_str().unwrap();
    let pinned = trekr(&db, &dir, &["--status", "--json", "--context", other_arg]);
    assert_eq!(pinned.status.code(), Some(0), "{}", stdout(&pinned));
    let from_there = json(&trekr(&db, &other, &["--status", "--json"]));
    assert_eq!(json(&pinned)["checkouts"], from_there["checkouts"]);
    let dir_arg = dir.to_str().unwrap();
    let unindexed = trekr(&db, &other, &["--status", "--context", dir_arg]);
    assert_eq!(unindexed.status.code(), Some(2));
    let missing = trekr(&db, &dir, &["--status", "--context", "no/such/dir"]);
    assert_eq!(missing.status.code(), Some(66));

    // Outside any checkout there is no "this checkout": the repos are listed.
    let (nowhere, _) = scratch("status-nowhere");
    let listed = trekr(&db, &nowhere, &["--status", "--json"]);
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(json(&listed)["checkouts"], from_there["checkouts"]);
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&other);
    let _ = fs::remove_dir_all(&nowhere);
}

/// A writer queued behind another says so on stderr rather than hanging
/// silently, and a JSON caller still gets only its one answer on stdout.
#[test]
fn a_queued_index_says_what_it_is_waiting_for() {
    let (dir, db) = scratch("queued");
    repo(&dir);
    assert!(trekr(&db, &dir, &["--index"]).status.success());
    fs::write(dir.join("gadget.rb"), "class Gadget\nend\n").unwrap();

    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut queued = spawn_trekr(&db, &dir, &["--index", "--json"]);
    let stderr = Said::of(&mut queued);
    stderr.wait_for("waiting for another");
    holder.execute_batch("ROLLBACK").unwrap();

    let out = queued.wait_with_output().unwrap();
    let stderr = stderr.all();
    assert!(out.status.success(), "{stderr}");
    assert_eq!(
        json(&out)["indexed"]["parsed"],
        1,
        "stdout is the answer alone"
    );
    let _ = fs::remove_dir_all(&dir);
}

/// A checkout an earlier index left cut short — the mark of a writer no
/// longer running — and a connection holding the write lock, so the next
/// `--index` of it cannot write a row until the holder lets go.
fn cut_short_behind_a_lock(label: &str) -> (PathBuf, PathBuf, rusqlite::Connection) {
    marked_behind_a_lock(label, i32::MAX as u32)
}

/// The same, with the mark naming `pid` as its writer.
fn marked_behind_a_lock(label: &str, pid: u32) -> (PathBuf, PathBuf, rusqlite::Connection) {
    let (dir, db) = scratch(label);
    repo(&dir);
    let indexed = trekr(&db, &dir, &["--index", "--json", "--no-gems"]);
    let root = json(&indexed)["repo"].as_str().unwrap().to_string();
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder
        .execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            [format!("warming {root}"), format!("{pid} 1 2")],
        )
        .unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    (dir, db, holder)
}

/// An index that cannot finish never exits 0: a script that trusts the exit
/// would answer from an index with most of the checkout missing (DEC-400).
#[test]
fn an_index_that_outwaits_the_lock_says_it_is_incomplete() {
    let (dir, db, holder) = cut_short_behind_a_lock("index-outwaited");
    let out = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index", "--json", "--no-gems"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_TEST_WRITER_WAIT_MS", "1500")
        .output()
        .unwrap();
    holder.execute_batch("ROLLBACK").unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("1 of 2 files"), "{stderr}");
    assert!(stderr.contains("trekr --index"), "{stderr}");
    let answer = json(&out);
    assert_eq!(answer["status"], "incomplete", "{answer}");
    assert_eq!(answer["warming"]["interrupted"], true, "{answer}");

    let text = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index", "--no-gems"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .output()
        .unwrap();
    assert!(text.status.success(), "the lock is free again");
    let _ = fs::remove_dir_all(&dir);
}

/// Outwaited by a writer that is still filling the checkout, an index does
/// not call that writer's index interrupted: it is not.
#[test]
fn an_index_outwaited_by_a_live_writer_leaves_its_mark_running() {
    let (dir, db, holder) = marked_behind_a_lock("index-outwaited-live", std::process::id());
    let out = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index", "--json", "--no-gems"])
        .current_dir(&dir)
        .env("TREKR_DB", &db)
        .env("TREKR_TEST_WRITER_WAIT_MS", "1500")
        .output()
        .unwrap();
    holder.execute_batch("ROLLBACK").unwrap();
    let answer = json(&out);
    assert_eq!(answer["status"], "incomplete", "{answer}");
    assert_eq!(answer["warming"]["interrupted"], false, "{answer}");
    let _ = fs::remove_dir_all(&dir);
}

/// Stopped by a signal — Ctrl-C, or a caller's timeout — an index says how
/// far it got before it goes, and dies of the signal, as a shell expects.
#[test]
fn an_index_stopped_by_a_signal_says_it_is_incomplete() {
    use std::os::unix::process::ExitStatusExt;
    let (dir, db, holder) = cut_short_behind_a_lock("index-stopped");
    let mut queued = spawn_trekr(&db, &dir, &["--index", "--json", "--no-gems"]);
    let stderr = Said::of(&mut queued);
    stderr.wait_for("waiting for another");
    // SAFETY: signals our own child, not yet reaped, so its pid is not reused.
    unsafe { libc::kill(queued.id() as i32, libc::SIGTERM) };
    let out = queued.wait_with_output().unwrap();
    holder.execute_batch("ROLLBACK").unwrap();
    let stderr = stderr.all();
    assert_eq!(out.status.signal(), Some(libc::SIGTERM), "{stderr}");
    assert!(stderr.contains("1 of 2 files"), "{stderr}");
    assert_eq!(json(&out)["status"], "incomplete");
    let _ = fs::remove_dir_all(&dir);
}

/// Ctrl-C on `trekr --index --json | tail` reaches both: the reader is gone
/// by the time the report is written, and the closed pipe is no panic.
#[test]
fn an_index_stopped_with_its_reader_gone_dies_quietly() {
    use std::os::unix::process::ExitStatusExt;
    let (dir, db, holder) = cut_short_behind_a_lock("index-stopped-pipe");
    let mut queued = spawn_trekr(&db, &dir, &["--index", "--json", "--no-gems"]);
    drop(queued.stdout.take());
    let stderr = Said::of(&mut queued);
    stderr.wait_for("waiting for another");
    // SAFETY: signals our own child, not yet reaped, so its pid is not reused.
    unsafe { libc::kill(queued.id() as i32, libc::SIGINT) };
    let out = queued.wait_with_output().unwrap();
    holder.execute_batch("ROLLBACK").unwrap();
    let stderr = stderr.all();
    assert_eq!(out.status.signal(), Some(libc::SIGINT), "{stderr}");
    assert!(!stderr.contains("panicked"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
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

/// A Ruby installed as rvm installs one, under `home`: its stdlib holding
/// `files`, and a default gem spec for each `(name, version, files)`.
/// Returns the stdlib's root, canonical as trekr stores it.
fn fake_ruby(
    home: &Path,
    version: &str,
    files: &[(&str, &str)],
    defaults: &[(&str, &str, &[&str])],
) -> String {
    fake_ruby_at(
        &home.join(format!(".rvm/rubies/ruby-{version}")),
        version,
        files,
        defaults,
    )
}

/// A Ruby installed at `prefix`, as `fake_ruby` stages one.
fn fake_ruby_at(
    prefix: &Path,
    version: &str,
    files: &[(&str, &str)],
    defaults: &[(&str, &str, &[&str])],
) -> String {
    let abi = {
        let mut parts = version.split('.');
        format!("{}.{}.0", parts.next().unwrap(), parts.next().unwrap())
    };
    let lib = prefix.join("lib/ruby");
    let stdlib = lib.join(&abi);
    fs::create_dir_all(&stdlib).unwrap();
    for (path, source) in files {
        let path = stdlib.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, source).unwrap();
    }
    let specs = lib.join("gems").join(&abi).join("specifications/default");
    fs::create_dir_all(&specs).unwrap();
    // It carries the fixture's signatures, as a real Ruby carries rbs.
    let gems = lib.join("gems").join(&abi).join("gems");
    fs::create_dir_all(&gems).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
        gems.join("rbs-9.9.9"),
    )
    .unwrap();
    for (name, version, owned) in defaults {
        let listed: Vec<String> = owned.iter().map(|f| format!("{f:?}.freeze")).collect();
        fs::write(
            specs.join(format!("{name}-{version}.gemspec")),
            format!(
                "Gem::Specification.new do |s|\n  s.name = {name:?}.freeze\n  \
                 s.files = [{}]\nend\n",
                listed.join(", ")
            ),
        )
        .unwrap();
    }
    fs::canonicalize(stdlib)
        .unwrap()
        .to_string_lossy()
        .into_owned()
}

/// An app on Ruby `version`, with a lockfile naming `specs` (`name (version)`
/// lines), and a vendored gem for each of `vendored`.
fn ruby_app(dir: &Path, version: &str, specs: &[&str], vendored: &[(&str, &str, &str)]) {
    repo(dir);
    fs::write(dir.join(".ruby-version"), format!("{version}\n")).unwrap();
    let mut lock = String::from("GEM\n  remote: https://rubygems.org/\n  specs:\n");
    for spec in specs {
        lock.push_str(&format!("    {spec}\n"));
    }
    lock.push_str("\nDEPENDENCIES\n");
    fs::write(dir.join("Gemfile.lock"), lock).unwrap();
    for (gem, path, source) in vendored {
        let file = dir.join(format!("vendor/bundle/ruby/9.8.0/gems/{gem}/lib/{path}"));
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, source).unwrap();
    }
}

fn definition_roots(answer: &serde_json::Value) -> Vec<String> {
    answer["definition"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|site| site["root"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// A checkout runs on a Ruby, and that Ruby's stdlib is indexed once, as a
/// checkout of its own, with its tooling left out (DEC-180).
#[test]
fn the_stdlib_of_the_apps_ruby_answers_its_calls() {
    let (dir, db) = scratch("stdlib");
    let (home, _) = scratch("stdlib-home");
    let stdlib = fake_ruby(
        &home,
        "9.8.7",
        &[
            ("stash.rb", "class Stash\n  def put(item)\n  end\nend\n"),
            ("irb.rb", "class Binding\n  def irb\n  end\nend\n"),
        ],
        &[],
    );
    ruby_app(&dir, "9.8.7", &[], &[]);
    let env = [("HOME", home.to_str().unwrap())];

    let first = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
    let reported = &first["gems"]["stdlib"];
    assert_eq!(reported["root"], stdlib.as_str(), "{first}");
    assert_eq!(reported["indexed"], true, "{first}");
    assert_eq!(reported["files"], 1, "irb is tooling, not indexed: {first}");
    assert!(
        reported["ruby"].as_str().unwrap().contains("9.8.7"),
        "{first}"
    );

    let answer = json(&trekr_env(&db, &dir, &["Stash#put", "--json"], &env));
    assert_eq!(definition_roots(&answer), [stdlib.as_str()], "{answer}");

    // Shared, as a gem is: a second index reads nothing again.
    let again = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
    assert_eq!(again["gems"]["stdlib"]["indexed"], false, "{again}");

    let status = json(&trekr_env(&db, &dir, &["--status", "--json"], &env));
    let rows = status["checkouts"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "the stdlib is not a repo: {status}");
    assert_eq!(rows[0]["stdlib"]["root"], stdlib.as_str(), "{status}");
    assert_eq!(rows[0]["gems"]["count"], 0, "nor a gem: {status}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// An app that bundles its own copy of a default gem sees only that copy; an
/// app that does not sees the stdlib's — whichever indexed last (DEC-180).
#[test]
fn a_bundled_default_gem_hides_the_stdlibs_copy_for_that_app_only() {
    let (plain, db) = scratch("stdlib-plain");
    let (bundling, _) = scratch("stdlib-bundling");
    let (home, _) = scratch("stdlib-shadow-home");
    let jsonish = "module Jsonish\n  def self.parse(text)\n  end\nend\n";
    let stdlib = fake_ruby(
        &home,
        "9.8.7",
        &[("jsonish.rb", jsonish)],
        &[("jsonish", "1.0.0", &["README.md", "jsonish.rb"])],
    );
    ruby_app(&plain, "9.8.7", &[], &[]);
    ruby_app(
        &bundling,
        "9.8.7",
        &["jsonish (2.0.0)"],
        &[("jsonish-2.0.0", "jsonish.rb", jsonish)],
    );
    let env = [("HOME", home.to_str().unwrap())];
    let owners = |app: &Path| {
        // Every place the module is written: a copy the app does not load
        // would be one more.
        let answer = json(&trekr_env(&db, app, &["Jsonish", "--json"], &env));
        let mut roots = definition_roots(&answer);
        roots.dedup();
        roots
    };

    trekr_env(&db, &plain, &["--index"], &env);
    let index = json(&trekr_env(&db, &bundling, &["--index", "--json"], &env));
    assert_eq!(
        index["gems"]["stdlib"]["hidden"],
        serde_json::json!(["jsonish"]),
        "{index}"
    );
    let gem = owners(&bundling);
    assert_eq!(gem.len(), 1, "one copy of Jsonish, not two: {gem:?}");
    assert!(gem[0].ends_with("jsonish-2.0.0"), "{gem:?}");
    assert_eq!(owners(&plain), [stdlib.as_str()]);

    // The other order: who indexed last changes neither answer.
    trekr_env(&db, &plain, &["--index"], &env);
    assert_eq!(owners(&bundling), gem);
    assert_eq!(owners(&plain), [stdlib]);

    for dir in [&plain, &bundling, &home] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// Core is read from the rbs gem the checkout's Ruby carries, once per Ruby,
/// and the index says which; a Ruby with none has no core, and says that
/// too (DEC-240).
#[test]
fn core_comes_from_the_rubys_rbs_and_a_ruby_without_one_has_none() {
    let (dir, db) = scratch("rbs-core");
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    let index = json(&trekr(&db, &dir, &["--index", "--json"]));
    let rbs = &index["gems"]["stdlib"]["rbs"];
    assert_eq!(rbs["version"], "9.9.9", "{index}");
    assert!(
        rbs["path"].as_str().unwrap().ends_with("rbs-9.9.9"),
        "{index}"
    );
    assert_eq!(rbs["read"], true, "{index}");
    assert_eq!(
        rbs["chosen"], "installed",
        "no gemspec beside the Ruby's own: {index}"
    );
    let again = json(&trekr(&db, &dir, &["--index", "--json"]));
    assert_eq!(
        again["gems"]["stdlib"]["rbs"]["read"], false,
        "read once: {again}"
    );
    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert_eq!(
        status["checkouts"][0]["stdlib"]["rbs"]["version"], "9.9.9",
        "{status}"
    );
    let upcase = trekr(&db, &dir, &["--def", "use.rb:1:5", "--json"]);
    assert_eq!(json(&upcase)["owner"], "String");

    // The same checkout on a Ruby that carries no rbs gem.
    let (bare, bare_db) = scratch("rbs-none");
    repo(&bare);
    fs::write(bare.join("use.rb"), "\"a\".upcase\n").unwrap();
    let (home, _) = scratch("rbs-none-home");
    let lib = home.join(".rvm/rubies/ruby-9.8.7/lib/ruby");
    fs::create_dir_all(lib.join("9.8.0")).unwrap();
    fs::create_dir_all(lib.join("gems/9.8.0/specifications/default")).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    let index = json(&trekr_env(&bare_db, &bare, &["--index", "--json"], &env));
    assert!(index["gems"]["stdlib"]["root"].is_string(), "{index}");
    assert!(index["gems"]["stdlib"]["rbs"].is_null(), "{index}");
    let text = stdout(&trekr_env(&bare_db, &bare, &["--index"], &env));
    assert!(text.contains("carries no rbs gem"), "{text}");
    let status = stdout(&trekr_env(&bare_db, &bare, &["--status"], &env));
    assert!(status.contains("no signatures"), "{status}");
    let upcase = trekr_env(&bare_db, &bare, &["--def", "use.rb:1:5", "--json"], &env);
    assert_eq!(upcase.status.code(), Some(1), "nothing is known of core");
    assert_eq!(json(&upcase)["status"], "residue");

    for dir in [&dir, &bare, &home] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A checkout that names no Ruby still runs on one — the `ruby` on `PATH`,
/// else the only Ruby installed — and its core is that Ruby's; with none of
/// them, nothing is known of core (DEC-242).
#[test]
fn a_checkout_naming_no_ruby_runs_on_the_one_it_finds() {
    let (dir, db) = scratch("rbs-unnamed");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    let (empty, _) = scratch("rbs-unnamed-home");

    // No Ruby named, none on `PATH`, none installed: none.
    let none = [("HOME", empty.to_str().unwrap())];
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &none));
    assert!(index["gems"].get("stdlib").is_none(), "{index}");
    let text = stdout(&trekr_env(&db, &dir, &["--index"], &none));
    assert!(text.contains("no Ruby found"), "{text}");
    let upcase = trekr_env(&db, &dir, &["--def", "use.rb:1:5", "--json"], &none);
    assert_eq!(json(&upcase)["status"], "residue");

    // The `ruby` on `PATH`, resolved to its prefix.
    let (prefix, _) = scratch("rbs-unnamed-prefix");
    let lib = prefix.join("lib/ruby");
    fs::create_dir_all(lib.join("9.7.0")).unwrap();
    fs::create_dir_all(lib.join("gems/9.7.0/specifications/default")).unwrap();
    fs::create_dir_all(lib.join("gems/9.7.0/gems")).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
        lib.join("gems/9.7.0/gems/rbs-9.9.9"),
    )
    .unwrap();
    fs::create_dir_all(prefix.join("bin")).unwrap();
    fs::write(prefix.join("bin/ruby"), "").unwrap();
    let path = format!("{}:{}", git_only().display(), prefix.join("bin").display());
    let on_path = [("HOME", empty.to_str().unwrap()), ("PATH", path.as_str())];
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &on_path));
    let stdlib = &index["gems"]["stdlib"];
    assert!(
        stdlib["ruby"].as_str().unwrap().contains("$PATH"),
        "{index}"
    );
    assert_eq!(stdlib["rbs"]["version"], "9.9.9", "{index}");
    let upcase = trekr_env(&db, &dir, &["--def", "use.rb:1:5", "--json"], &on_path);
    assert_eq!(json(&upcase)["owner"], "String");

    // Nothing on `PATH`, one Ruby installed: that one.
    let (db2, _) = (db.with_extension("two.db"), ());
    let index = json(&trekr(&db2, &dir, &["--index", "--json"]));
    assert!(
        index["gems"]["stdlib"]["ruby"]
            .as_str()
            .unwrap()
            .contains("the only Ruby installed"),
        "{index}"
    );

    for dir in [&dir, &empty, &prefix] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A checkout that names no Ruby falls back through what else says which —
/// its lockfile, the version manager, the environment, the highest installed
/// that meets its gemspec — to the first Ruby that carries signatures, and
/// says it is a fallback (DEC-610).
#[test]
fn a_checkout_naming_no_ruby_falls_back_to_one_with_signatures() {
    let (parent, store) = scratch("ruby-fallback");
    fs::remove_file(parent.join(".ruby-version")).unwrap();
    let dir = parent.join("app");
    fs::create_dir_all(&dir).unwrap();
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    fs::write(
        dir.join("widget.gemspec"),
        "Gem::Specification.new do |s|\n  s.name = \"widget\"\n  \
         s.required_ruby_version = \">= 9.5\"\nend\n",
    )
    .unwrap();
    let (home, _) = scratch("ruby-fallback-home");
    let versions = home.join(".rbenv/versions");
    let low = fake_ruby_at(&versions.join("9.6.1"), "9.6.1", &[], &[]);
    let high = fake_ruby_at(&versions.join("9.7.1"), "9.7.1", &[], &[]);
    let old = fake_ruby_at(&versions.join("9.4.2"), "9.4.2", &[], &[]);
    // The `ruby` on `PATH` is a system Ruby with no rbs, as macOS's is.
    let (system, _) = scratch("ruby-fallback-system");
    let lib = system.join("lib/ruby");
    fs::create_dir_all(lib.join("9.5.0")).unwrap();
    fs::create_dir_all(lib.join("gems/9.5.0/specifications/default")).unwrap();
    fs::create_dir_all(system.join("bin")).unwrap();
    fs::write(system.join("bin/ruby"), "").unwrap();
    let path = format!("{}:{}", git_only().display(), system.join("bin").display());

    let base = [("HOME", home.to_str().unwrap()), ("PATH", path.as_str())];
    // A store each, or the last index's Ruby would be kept (DEC-271).
    let mut stores = 0;
    let mut index = |vars: &[(&str, &str)]| {
        stores += 1;
        let db = store.with_extension(format!("{stores}.db"));
        let env = [&base[..], vars].concat();
        let out = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
        (db, out)
    };

    // Nothing names one, `PATH`'s carries no rbs: the highest installed
    // that meets the gemspec.
    let (db, out) = index(&[]);
    assert_eq!(out["ruby"]["root"], high.as_str(), "{out}");
    assert_eq!(out["ruby"]["how"], "highest", "{out}");
    assert_eq!(out["ruby"]["fallback"], true, "{out}");
    let said = out["gems"]["stdlib"]["ruby"].as_str().unwrap();
    assert!(said.starts_with("Ruby 9.7.1 (fallback: "), "{said}");
    assert!(said.contains(">= 9.5") && said.contains("$PATH"), "{said}");
    let env = base;
    let upcase = trekr_env(&db, &dir, &["--def", "use.rb:1:5", "--json"], &env);
    assert_eq!(json(&upcase)["owner"], "String", "core is known");
    let status = json(&trekr_env(&db, &dir, &["--status", "--json"], &env));
    assert_eq!(status["checkouts"][0]["ruby"], out["ruby"], "{status}");
    let status = stdout(&trekr_env(&db, &dir, &["--status"], &env));
    assert!(status.contains("Ruby 9.7.1 (fallback: "), "{status}");

    // The gemspec rules out the higher.
    fs::write(
        dir.join("widget.gemspec"),
        "Gem::Specification.new do |s|\n  s.required_ruby_version = [\">= 9.5\", \"< 9.7\"]\nend\n",
    )
    .unwrap();
    let (_, out) = index(&[]);
    assert_eq!(out["ruby"]["root"], low.as_str(), "{out}");

    // The version manager's choice, from its variable or its global file —
    // but not one the gemspec rules out.
    let (_, out) = index(&[("RBENV_VERSION", "9.6.1")]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(low.as_str()), Some("manager")),
        "{out}"
    );
    assert!(
        out["gems"]["stdlib"]["ruby"]
            .as_str()
            .unwrap()
            .contains("$RBENV_VERSION"),
        "{out}"
    );
    fs::write(home.join(".rbenv/version"), "9.4.2\n").unwrap();
    let (_, out) = index(&[]);
    assert_eq!(out["ruby"]["root"], low.as_str(), "outside >= 9.5: {out}");
    fs::write(dir.join("widget.gemspec"), "").unwrap();
    let (_, out) = index(&[]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(old.as_str()), Some("manager")),
        "{out}"
    );
    // The shell's current Ruby outranks a manager's global.
    let gem_home = format!("{}/lib/ruby/gems/9.6.0", versions.join("9.6.1").display());
    let (_, out) = index(&[("GEM_HOME", gem_home.as_str())]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(low.as_str()), Some("gem_home")),
        "{out}"
    );
    fs::remove_file(home.join(".rbenv/version")).unwrap();

    // A `.ruby-version` above the checkout, as rbenv finds one.
    fs::write(parent.join(".ruby-version"), "9.6\n").unwrap();
    let (_, out) = index(&[]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(low.as_str()), Some("manager")),
        "{out}"
    );
    fs::remove_file(parent.join(".ruby-version")).unwrap();

    // The lockfile's `RUBY VERSION`, ahead of the environment.
    fs::write(
        dir.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n\nRUBY VERSION\n   ruby 9.4.2p100\n\n\
         BUNDLED WITH\n   2.5.0\n",
    )
    .unwrap();
    let (_, out) = index(&[("RBENV_VERSION", "9.6.1")]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(old.as_str()), Some("lockfile")),
        "{out}"
    );
    fs::remove_file(dir.join("Gemfile.lock")).unwrap();

    // What the checkout names is no fallback.
    fs::write(dir.join(".tool-versions"), "nodejs 20.1.0\nruby 9.6.1\n").unwrap();
    let (_, out) = index(&[]);
    assert_eq!(
        (
            out["ruby"]["root"].as_str(),
            out["ruby"]["fallback"].as_bool()
        ),
        (Some(low.as_str()), Some(false)),
        "{out}"
    );

    for dir in [&parent, &home, &system] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// What the checkout writes outranks what its last index chose: its
/// lockfile's `RUBY VERSION` moves a kept Ruby, counting by its minor when
/// that patch is not installed, and a requirement the kept one no longer
/// meets moves it too (DEC-610).
#[test]
fn the_checkouts_lockfile_and_requirement_move_a_kept_ruby() {
    let (dir, db) = scratch("ruby-kept-moves");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    let (home, _) = scratch("ruby-kept-moves-home");
    fs::remove_file(home.join(".ruby-version")).unwrap();
    let versions = home.join(".rbenv/versions");
    let old = fake_ruby_at(&versions.join("9.4.2"), "9.4.2", &[], &[]);
    let new = fake_ruby_at(&versions.join("9.6.3"), "9.6.3", &[], &[]);
    let env = [("HOME", home.to_str().unwrap())];
    let lock = |ruby: &str| {
        fs::write(
            dir.join("Gemfile.lock"),
            format!(
                "GEM\n  remote: https://rubygems.org/\n  specs:\n\nRUBY VERSION\n   ruby {ruby}\n\n\
                 BUNDLED WITH\n   2.5.0\n"
            ),
        )
        .unwrap();
    };
    let index = || {
        let out = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
        let said = out["gems"]["stdlib"]["ruby"].as_str().unwrap().to_string();
        (
            out["ruby"]["root"].as_str().unwrap().to_string(),
            out["ruby"]["how"].clone(),
            said,
        )
    };

    lock("9.4.2p100");
    let (root, how, _) = index();
    assert_eq!(
        (root.as_str(), how.as_str()),
        (old.as_str(), Some("lockfile"))
    );

    // A patch this machine lacks: the highest of its minor, and that is said.
    lock("9.6.1p0");
    let (root, how, said) = index();
    assert_eq!(
        (root.as_str(), how.as_str()),
        (new.as_str(), Some("lockfile")),
        "{said}"
    );
    assert!(
        said.contains("9.6.1") && said.contains("not installed"),
        "{said}"
    );

    // The kept Ruby falls outside a requirement the checkout now writes.
    lock("9.4.2p100");
    assert_eq!(index().0, old);
    fs::remove_file(dir.join("Gemfile.lock")).unwrap();
    fs::write(
        dir.join("widget.gemspec"),
        "Gem::Specification.new do |s|\n  s.required_ruby_version = \">= 9.5\"\nend\n",
    )
    .unwrap();
    let (root, how, said) = index();
    assert_eq!(
        (root.as_str(), how.as_str()),
        (new.as_str(), Some("highest")),
        "{said}"
    );
    assert!(said.contains("in place of"), "{said}");

    for dir in [&dir, &home] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A Ruby kept from the last index that meets the checkout's requirement is
/// not reported unmet in an environment that finds no Ruby, and two installs
/// of one version passed over are said once (DEC-610).
#[test]
fn a_kept_ruby_meeting_the_requirement_is_not_unmet() {
    let (dir, db) = scratch("ruby-kept-met");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    fs::write(
        dir.join("widget.gemspec"),
        "Gem::Specification.new do |s|\n  s.required_ruby_version = \">= 9.7\"\nend\n",
    )
    .unwrap();
    let (home, _) = scratch("ruby-kept-met-home");
    fs::remove_file(home.join(".ruby-version")).unwrap();
    let high = fake_ruby_at(&home.join(".rbenv/versions/9.7.1"), "9.7.1", &[], &[]);
    // One version twice, each reached before the highest: chruby's current
    // Ruby, and the one `$GEM_HOME` is in.
    fake_ruby_at(&home.join(".rbenv/versions/9.6.1"), "9.6.1", &[], &[]);
    fake_ruby_at(&home.join(".rubies/9.6.1"), "9.6.1", &[], &[]);
    let rubies = home.join(".rubies/9.6.1");
    let gem_home = home.join(".rbenv/versions/9.6.1/lib/ruby/gems/9.6.0");
    let rich = [
        ("HOME", home.to_str().unwrap()),
        ("RUBY_ROOT", rubies.to_str().unwrap()),
        ("GEM_HOME", gem_home.to_str().unwrap()),
    ];
    let out = json(&trekr_env(&db, &dir, &["--index", "--json"], &rich));
    assert_eq!(out["ruby"]["root"], high.as_str(), "{out}");
    let said = out["gems"]["stdlib"]["ruby"].as_str().unwrap();
    assert_eq!(said.matches("Ruby 9.6.1").count(), 1, "{said}");
    assert!(!said.contains("those installed"), "{said}");

    // An editor launched from the Dock: no Rubies found, the kept one meets.
    let (poor, _) = scratch("ruby-kept-met-poor");
    let poor = [("HOME", poor.to_str().unwrap())];
    let out = json(&trekr_env(&db, &dir, &["--index", "--json"], &poor));
    assert_eq!(out["ruby"]["root"], high.as_str(), "{out}");
    assert!(out["gems"].get("ruby_unmet").is_none(), "{out}");
    let status = json(&trekr_env(&db, &dir, &["--status", "--json"], &poor));
    assert!(
        status["checkouts"][0].get("ruby_unmet").is_none(),
        "{status}"
    );
    let text = stdout(&trekr_env(&db, &dir, &["--status"], &poor));
    assert!(!text.contains("no installed Ruby meets"), "{text}");
    for dir in [&dir, &home] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A fallback that could not take what the checkout or the environment asked
/// for says so: no installed Ruby meets the requirement, a manager's choice
/// that is not installed, a version file whose first word is no version
/// (DEC-610).
#[test]
fn a_ruby_fallback_says_what_it_could_not_take() {
    let (dir, db) = scratch("ruby-unmet");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    let (home, _) = scratch("ruby-unmet-home");
    fs::remove_file(home.join(".ruby-version")).unwrap();
    let versions = home.join(".rbenv/versions");
    let low = fake_ruby_at(&versions.join("9.6.1"), "9.6.1", &[], &[]);
    let high = fake_ruby_at(&versions.join("9.7.1"), "9.7.1", &[], &[]);
    // rvm's gemset suffix, and an install rbenv reaches through a link.
    let (elsewhere, _) = scratch("ruby-unmet-elsewhere");
    let linked = fake_ruby_at(&elsewhere.join("9.5.4"), "9.5.4", &[], &[]);
    std::os::unix::fs::symlink(elsewhere.join("9.5.4"), versions.join("9.5.4")).unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    // A store each, or the last index's Ruby would be kept (DEC-271).
    let mut stores = 0;
    let mut index = |vars: &[(&str, &str)]| {
        stores += 1;
        let db = db.with_extension(format!("{stores}.db"));
        let all = [&env[..], vars].concat();
        let out = json(&trekr_env(&db, &dir, &["--index", "--json"], &all));
        let text = stdout(&trekr_env(&db, &dir, &["--index"], &all));
        let status = json(&trekr_env(&db, &dir, &["--status", "--json"], &all));
        (out, text, status)
    };

    // Nothing installed meets the gemspec: the first found, and that is said.
    fs::write(
        dir.join("widget.gemspec"),
        "Gem::Specification.new do |s|\n  s.required_ruby_version = \">= 9.9\"\nend\n",
    )
    .unwrap();
    let (out, text, status) = index(&[]);
    assert_eq!(
        out["gems"]["ruby_unmet"], "widget.gemspec's >= 9.9",
        "{out}"
    );
    assert!(
        text.contains("no installed Ruby meets widget.gemspec's >= 9.9"),
        "{text}"
    );
    assert_eq!(
        status["checkouts"][0]["ruby_unmet"], "widget.gemspec's >= 9.9",
        "{status}"
    );
    assert!(!text.contains("those installed"), "{text}");
    // `--status` does not call it what the checkout allows, above the line
    // saying nothing installed meets it.
    let said = stdout(&trekr_env(
        &db.with_extension("1.db"),
        &dir,
        &["--status"],
        &env,
    ));
    assert!(said.contains("no installed Ruby meets"), "{said}");
    assert!(!said.contains("the checkout allows"), "{said}");
    fs::remove_file(dir.join("widget.gemspec")).unwrap();

    // The manager names one that is not installed: passed over, once.
    let (out, _, _) = index(&[("RBENV_VERSION", "9.9.9")]);
    let said = out["gems"]["stdlib"]["ruby"].as_str().unwrap();
    assert_eq!(out["ruby"]["root"], high.as_str(), "{out}");
    assert!(
        said.contains("$RBENV_VERSION, 9.9.9: not installed"),
        "{said}"
    );
    assert!(out["gems"].get("ruby_unmet").is_none(), "{out}");

    // `.ruby-version` of `system` leaves `.tool-versions` to name one.
    fs::write(dir.join(".ruby-version"), "system\n").unwrap();
    fs::write(dir.join(".tool-versions"), "ruby 9.6.1\n").unwrap();
    let (out, _, _) = index(&[]);
    assert_eq!(
        (out["ruby"]["root"].as_str(), out["ruby"]["how"].as_str()),
        (Some(low.as_str()), Some("named")),
        "{out}"
    );
    fs::remove_file(dir.join(".tool-versions")).unwrap();

    // Its first word is what a manager reads; rvm's gemset is not the version.
    for (written, root) in [
        ("9.6.1 # pinned\n", &low),
        ("ruby-9.6.1@widgets\n", &low),
        ("9.5.4\n", &linked),
    ] {
        fs::write(dir.join(".ruby-version"), written).unwrap();
        let (out, _, _) = index(&[]);
        assert_eq!(out["ruby"]["root"], root.as_str(), "{written}: {out}");
        assert_eq!(out["ruby"]["how"], "named", "{written}: {out}");
        assert!(
            out["gems"].get("ruby_not_found").is_none(),
            "{written}: {out}"
        );
    }

    for dir in [&dir, &home, &elsewhere] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// chruby's, mise's and Homebrew's versioned Rubies are found by the version
/// a checkout names; one that is not installed is said, with the Ruby run on
/// instead (DEC-270).
#[test]
fn a_named_ruby_is_found_wherever_a_version_manager_put_it() {
    let (dir, db) = scratch("ruby-managers");
    repo(&dir);
    let (home, _) = scratch("ruby-managers-home");
    let (system, _) = scratch("ruby-managers-system");
    let installs = [
        ("9.6.1", home.join(".rubies/ruby-9.6.1")),
        ("9.5.2", home.join(".local/share/mise/installs/ruby/9.5.2")),
        ("9.4.3", system.join("opt/homebrew/Cellar/ruby@9.4/9.4.3")),
        ("9.3.1", system.join("opt/rubies/ruby-9.3.1")),
    ];
    let roots: Vec<String> = installs
        .iter()
        .map(|(version, prefix)| fake_ruby_at(prefix, version, &[], &[]))
        .collect();
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("TREKR_TEST_SYSTEM", system.to_str().unwrap()),
    ];
    for ((version, _), root) in installs.iter().zip(&roots) {
        // `9.4` names the highest 9.4, as `.ruby-version` often does.
        let named = if *version == "9.4.3" { "9.4" } else { version };
        fs::write(dir.join(".ruby-version"), format!("{named}\n")).unwrap();
        let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
        assert_eq!(index["gems"]["stdlib"]["root"], root.as_str(), "{index}");
        assert!(index["gems"].get("ruby_not_found").is_none(), "{index}");
    }

    // Named, not installed: another Ruby answers, and that is said. A store
    // of its own, or the last index's Ruby would be kept (DEC-271).
    let db = db.with_extension("missing.db");
    fs::write(dir.join(".ruby-version"), "9.2\n").unwrap();
    let gem_home = format!("{}/lib/ruby/gems/9.6.0", installs[0].1.display());
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("TREKR_TEST_SYSTEM", system.to_str().unwrap()),
        ("GEM_HOME", gem_home.as_str()),
    ];
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
    assert_eq!(index["gems"]["ruby_not_found"], "9.2", "{index}");
    assert_eq!(
        index["gems"]["stdlib"]["root"],
        roots[0].as_str(),
        "{index}"
    );
    let text = stdout(&trekr_env(&db, &dir, &["--index"], &env));
    assert!(
        text.contains("names Ruby 9.2, which is not installed")
            && text.contains("running on Ruby 9.6.1 (fallback: the Ruby $GEM_HOME names"),
        "{text}"
    );
    let status = json(&trekr_env(&db, &dir, &["--status", "--json"], &env));
    assert_eq!(status["checkouts"][0]["ruby_not_found"], "9.2", "{status}");
    let status = stdout(&trekr_env(&db, &dir, &["--status"], &env));
    assert!(
        status.contains("names Ruby 9.2, which is not installed"),
        "{status}"
    );

    for dir in [&dir, &home, &system] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// A reindex in a poorer environment — an editor launched from the Dock, the
/// language server's background index — keeps the Ruby and signatures the
/// last one chose; only the checkout naming another Ruby moves them (DEC-271).
#[test]
fn a_reindex_in_a_poorer_environment_keeps_the_rubys_core() {
    let (dir, db) = scratch("ruby-kept");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    let (home, _) = scratch("ruby-kept-home");
    // Not a `~/.ruby-version` (DEC-610).
    fs::remove_file(home.join(".ruby-version")).unwrap();
    let first = fake_ruby(&home, "9.7.1", &[], &[]);
    let second = fake_ruby(&home, "9.8.7", &[], &[]);
    let gem_home = format!("{}/.rvm/gems/ruby-9.7.1", home.display());
    let rich = [
        ("HOME", home.to_str().unwrap()),
        ("GEM_HOME", gem_home.as_str()),
    ];
    // None named, none on `PATH`, no `$GEM_HOME`: its fallback is the
    // highest installed, not the shell's.
    let bare = [("HOME", home.to_str().unwrap())];

    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &rich));
    assert_eq!(index["gems"]["stdlib"]["root"], first.as_str(), "{index}");
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &bare));
    let stdlib = &index["gems"]["stdlib"];
    assert_eq!(stdlib["root"], first.as_str(), "{index}");
    assert!(
        stdlib["ruby"]
            .as_str()
            .unwrap()
            .contains("kept from the last index"),
        "{index}"
    );
    assert_eq!(stdlib["rbs"]["version"], "9.9.9", "{index}");
    let upcase = trekr_env(&db, &dir, &["--def", "use.rb:1:5", "--json"], &bare);
    assert_eq!(json(&upcase)["owner"], "String", "core is still known");

    // The checkout names another Ruby: that one, and it is said.
    fs::write(dir.join(".ruby-version"), "9.8.7\n").unwrap();
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &bare));
    let stdlib = &index["gems"]["stdlib"];
    assert_eq!(stdlib["root"], second.as_str(), "{index}");
    assert!(
        stdlib["ruby"].as_str().unwrap().contains("in place of"),
        "{index}"
    );

    // Signatures found only through one `$HOME` are kept under another.
    let (system, _) = scratch("ruby-kept-system");
    let (rbs_home, _) = scratch("ruby-kept-rbs-home");
    let (empty, _) = scratch("ruby-kept-empty");
    let prefix = system.join("opt/rubies/ruby-9.6.1");
    let lib = prefix.join("lib/ruby");
    fs::create_dir_all(lib.join("9.6.0")).unwrap();
    fs::create_dir_all(lib.join("gems/9.6.0/specifications/default")).unwrap();
    fs::create_dir_all(rbs_home.join(".gem/ruby/9.6.0/gems")).unwrap();
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
        rbs_home.join(".gem/ruby/9.6.0/gems/rbs-9.9.9"),
    )
    .unwrap();
    fs::write(dir.join(".ruby-version"), "9.6.1\n").unwrap();
    let with = |home: &Path| {
        let index = trekr_env(
            &db,
            &dir,
            &["--index", "--json"],
            &[
                ("HOME", home.to_str().unwrap()),
                ("TREKR_TEST_SYSTEM", system.to_str().unwrap()),
            ],
        );
        json(&index)["gems"]["stdlib"]["rbs"].clone()
    };
    let rbs = with(&rbs_home);
    assert_eq!(
        (rbs["version"].as_str(), rbs["chosen"].as_str()),
        (Some("9.9.9"), Some("installed")),
        "{rbs}"
    );
    let rbs = with(&empty);
    assert_eq!(rbs["version"], "9.9.9", "{rbs}");
    assert_eq!(rbs["kept"], true, "{rbs}");

    for dir in [&dir, &home, &system, &rbs_home, &empty] {
        let _ = fs::remove_dir_all(dir);
    }
}

/// The rbs bundled with a Ruby stays its choice through routine gem
/// maintenance: `gem update --system` writes a newer default spec, and `gem
/// pristine rbs` rewrites the gem's own (DEC-272).
#[test]
fn the_bundled_rbs_survives_gem_maintenance() {
    let (dir, db) = scratch("rbs-maintenance");
    repo(&dir);
    let (home, _) = scratch("rbs-maintenance-home");
    fake_ruby(&home, "9.8.7", &[], &[]);
    let base = home.join(".rvm/rubies/ruby-9.8.7/lib/ruby/gems/9.8.0");
    let touch = |path: &Path, secs: u64| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "").unwrap();
        fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
            .unwrap();
    };
    // Installed with its default gems and its rbs 9.9.9; 9.10.0 came later.
    for spec in ["json-2.9.1", "set-1.1.1", "uri-1.0.3"] {
        touch(
            &base.join(format!("specifications/default/{spec}.gemspec")),
            1_000_000,
        );
    }
    touch(&base.join("specifications/rbs-9.9.9.gemspec"), 1_000_004);
    touch(&base.join("cache/rbs-9.9.9.gem"), 900_000);
    std::os::unix::fs::symlink(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/rbs"),
        base.join("gems/rbs-9.10.0"),
    )
    .unwrap();
    touch(&base.join("specifications/rbs-9.10.0.gemspec"), 9_000_000);
    touch(&base.join("cache/rbs-9.10.0.gem"), 9_000_000);
    let env = [("HOME", home.to_str().unwrap())];
    let chosen = |db: &Path| {
        let index = json(&trekr_env(db, &dir, &["--index", "--json"], &env));
        let rbs = &index["gems"]["stdlib"]["rbs"];
        (
            rbs["version"].as_str().unwrap_or_default().to_string(),
            rbs["chosen"].as_str().unwrap_or_default().to_string(),
        )
    };
    let bundled = ("9.9.9".to_string(), "bundled".to_string());
    assert_eq!(chosen(&db), bundled);

    for spec in ["rubygems-update-9.0.0", "bundler-9.0.0"] {
        touch(
            &base.join(format!("specifications/default/{spec}.gemspec")),
            9_500_000,
        );
    }
    assert_eq!(
        chosen(&db.with_extension("update.db")),
        bundled,
        "gem update --system"
    );
    touch(&base.join("specifications/rbs-9.9.9.gemspec"), 9_600_000);
    assert_eq!(
        chosen(&db.with_extension("pristine.db")),
        bundled,
        "gem pristine rbs"
    );

    // Reinstalled at the same version and path: read again.
    let again = |db: &Path| {
        json(&trekr_env(db, &dir, &["--index", "--json"], &env))["gems"]["stdlib"]["rbs"]["read"]
            .clone()
    };
    again(&db);
    assert_eq!(again(&db), false, "already known");
    touch(&base.join("specifications/rbs-9.9.9.gemspec"), 9_700_000);
    assert_eq!(again(&db), true, "a reinstall is read again");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// With no Ruby, a name nothing defines says core was never looked in, rather
/// than claiming core lacks it.
#[test]
fn a_miss_without_core_says_core_is_not_indexed() {
    let (dir, db) = scratch("no-core-reason");
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    repo(&dir);
    fs::write(dir.join("use.rb"), "\"a\".upcase\n").unwrap();
    let (empty, _) = scratch("no-core-reason-home");
    let none = [("HOME", empty.to_str().unwrap())];
    trekr_env(&db, &dir, &["--index"], &none);
    let upcase = json(&trekr_env(
        &db,
        &dir,
        &["--def", "use.rb:1:5", "--json"],
        &none,
    ));
    let reason = upcase["reason"].as_str().unwrap();
    assert!(
        reason.contains("Ruby core is not indexed for this checkout"),
        "{upcase}"
    );
    assert!(!reason.contains("its gems or Ruby core"), "{upcase}");
    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&empty);
}

/// A lockfile naming a default gem at the version its Ruby ships names the
/// stdlib's copy: found, not missing (DEC-180).
#[test]
fn a_default_gem_at_the_rubys_own_version_is_found_in_the_stdlib() {
    let (dir, db) = scratch("stdlib-default");
    let (home, _) = scratch("stdlib-default-home");
    fake_ruby(
        &home,
        "9.8.7",
        &[("jsonish.rb", "module Jsonish\nend\n")],
        &[("jsonish", "1.0.0", &["jsonish.rb"])],
    );
    ruby_app(&dir, "9.8.7", &["jsonish (1.0.0)"], &[]);
    let env = [("HOME", home.to_str().unwrap())];
    let gems = json(&trekr_env(&db, &dir, &["--index", "--json"], &env))["gems"].clone();
    assert_eq!(gems["from_stdlib"], 1, "{gems}");
    assert!(gems.get("missing").is_none(), "{gems}");
    assert_eq!(gems["stdlib"]["hidden"], serde_json::json!([]), "{gems}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// The checkout's Ruby is where its gems are looked for first, lockfile or
/// not: a default gem it ships answers from its stdlib, and another Ruby's
/// copy is taken only for a gem it lacks, which is said (DEC-291).
#[test]
fn gems_come_from_the_checkouts_ruby_before_the_shells() {
    let (dir, db) = scratch("gems-ruby");
    let (home, _) = scratch("gems-ruby-home");
    let prefix = home.join(".rbenv/versions/9.7.1");
    let stdlib = fake_ruby_at(
        &prefix,
        "9.7.1",
        &[(
            "securely.rb",
            "module Securely
  def self.hex
  end
end
",
        )],
        &[("securely", "0.4.1", &["securely.rb"])],
    );
    let own = prefix.join("lib/ruby/gems/9.7.0/gems");
    let shell = home.join(".rvm/gems/ruby-9.6.1");
    for (gems, gem, file, module) in [
        (&own, "widget-1.0.0", "widget.rb", "Widget"),
        (&shell.join("gems"), "widget-1.0.0", "widget.rb", "Widget"),
        (
            &shell.join("gems"),
            "securely-0.4.1",
            "securely.rb",
            "Securely",
        ),
        (&shell.join("gems"), "other-2.0.0", "other.rb", "Other"),
    ] {
        let lib = gems.join(gem).join("lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(lib.join(file), format!("module {module}\nend\n")).unwrap();
    }
    ruby_app(
        &dir,
        "9.7.1",
        &["other (2.0.0)", "securely (0.4.1)", "widget (1.0.0)"],
        &[],
    );
    fs::write(dir.join("use.rb"), "Widget\nSecurely\nOther\n").unwrap();
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("GEM_HOME", shell.to_str().unwrap()),
    ];
    let root_of = |line: u32| {
        let at = format!("use.rb:{line}:1");
        let answer = json(&trekr_env(&db, &dir, &["--def", &at, "--json"], &env));
        answer["definition"][0]["root"]
            .as_str()
            .unwrap_or_else(|| panic!("{answer}"))
            .to_string()
    };
    let own_widget = fs::canonicalize(own.join("widget-1.0.0")).unwrap();

    let gems = json(&trekr_env(&db, &dir, &["--index", "--json"], &env))["gems"].clone();
    assert_eq!(gems["from_stdlib"], 1, "{gems}");
    assert_eq!(
        gems["other_ruby"],
        serde_json::json!(["other 2.0.0"]),
        "{gems}"
    );
    assert_eq!(root_of(1), own_widget.to_str().unwrap());
    assert_eq!(root_of(2), stdlib, "its Ruby ships it");
    let text = stdout(&trekr_env(&db, &dir, &["--index"], &env));
    assert!(
        text.contains("another Ruby") && text.contains("other 2.0.0"),
        "{text}"
    );

    // Without a lockfile, the picks are that Ruby's too; one it lacks comes
    // from the shell's, and is said.
    fs::remove_file(dir.join("Gemfile.lock")).unwrap();
    fs::write(
        dir.join("app.gemspec"),
        "Gem::Specification.new do |s|\n  s.add_dependency \"widget\"\n  \
         s.add_dependency \"other\"\nend\n",
    )
    .unwrap();
    let gems = json(&trekr_env(&db, &dir, &["--index", "--json"], &env))["gems"].clone();
    assert_eq!(gems["resolved_from"], "declared", "{gems}");
    assert!(gems["ruby"].as_str().unwrap().contains("9.7.1"), "{gems}");
    assert!(gems.get("missing").is_none(), "{gems}");
    assert_eq!(
        gems["other_ruby"],
        serde_json::json!(["other 2.0.0"]),
        "{gems}"
    );
    assert_eq!(root_of(1), own_widget.to_str().unwrap());

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// `--index` and `--status` name the checkout's Ruby as an object — its
/// version, its stdlib's root, and how it was chosen — beside the sentence
/// (DEC-292).
#[test]
fn the_rubys_choice_is_structured_in_index_and_status() {
    let (dir, db) = scratch("ruby-object");
    repo(&dir);
    let (home, _) = scratch("ruby-object-home");
    // Not a `~/.ruby-version` (DEC-610).
    fs::remove_file(home.join(".ruby-version")).unwrap();
    let first = fake_ruby(&home, "9.8.7", &[], &[]);
    let second = fake_ruby(&home, "9.7.1", &[], &[]);
    let gem_home = format!("{}/.rvm/gems/ruby-9.8.7", home.display());
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("GEM_HOME", gem_home.as_str()),
    ];
    let ruby = |db: &Path, args: &[&str]| json(&trekr_env(db, &dir, args, &env));

    fs::write(dir.join(".ruby-version"), "9.7.1\n").unwrap();
    let index = ruby(&db, &["--index", "--json"]);
    assert_eq!(
        index["ruby"],
        serde_json::json!({ "version": "9.7.1", "root": second, "how": "named", "fallback": false }),
        "{index}"
    );
    assert!(
        index["gems"]["stdlib"]["ruby"].is_string(),
        "the sentence stays"
    );

    // Named no longer, and `$GEM_HOME` names another: the last one is kept.
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    let index = ruby(&db, &["--index", "--json"]);
    assert_eq!(index["ruby"]["how"], "kept", "{index}");
    assert_eq!(index["ruby"]["root"], second.as_str(), "{index}");
    let status = ruby(&db, &["--status", "--json"]);
    assert_eq!(status["checkouts"][0]["ruby"], index["ruby"], "{status}");

    // A store of its own: `$GEM_HOME`'s.
    let fresh = db.with_extension("fresh.db");
    let index = ruby(&fresh, &["--index", "--json"]);
    assert_eq!(
        index["ruby"],
        serde_json::json!({ "version": "9.8.7", "root": first, "how": "gem_home", "fallback": true }),
        "{index}"
    );
    let skipped = ruby(&fresh, &["--index", "--no-gems", "--json"]);
    assert!(skipped["ruby"].is_null(), "{skipped}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// Without a lockfile, a Gemfile's git gem is its one checkout in
/// `bundler/gems`; one with several or none is said, never dropped (DEC-293).
#[test]
fn without_a_lockfile_a_git_gem_is_its_one_checkout_or_said() {
    let (dir, db) = scratch("declared-git");
    repo(&dir);
    let (home, _) = scratch("declared-git-home");
    let prefix = home.join(".rvm/rubies/ruby-9.8.7");
    fake_ruby_at(&prefix, "9.8.7", &[], &[]);
    let checkouts = prefix.join("lib/ruby/gems/9.8.0/bundler/gems");
    for (checkout, gem) in [
        ("kit-0123456789ab", "kit"),
        ("other-aaaaaaaaaaaa", "other"),
        ("other-bbbbbbbbbbbb", "other"),
    ] {
        let lib = checkouts.join(checkout).join("lib");
        fs::create_dir_all(&lib).unwrap();
        fs::write(
            checkouts.join(checkout).join(format!("{gem}.gemspec")),
            "Gem::Specification.new do |s|\nend\n",
        )
        .unwrap();
        let module = if gem == "kit" { "Kit" } else { "Other" };
        fs::write(
            lib.join(format!("{gem}.rb")),
            format!("module {module}\nend\n"),
        )
        .unwrap();
    }
    fs::write(
        dir.join("Gemfile"),
        "gem \"kit\", github: \"acme/kit\"\n\
         gem \"other\", github: \"acme/other\"\n\
         gem \"gone\", git: \"https://example.com/acme/gone.git\"\n",
    )
    .unwrap();
    fs::write(dir.join("use.rb"), "Kit\n").unwrap();
    let env = [("HOME", home.to_str().unwrap())];

    let gems = json(&trekr_env(&db, &dir, &["--index", "--json"], &env))["gems"].clone();
    assert_eq!(
        gems["picked"],
        serde_json::json!(["kit 0123456789ab"]),
        "{gems}"
    );
    assert_eq!(gems["from_git"], 1, "{gems}");
    let why = |gem: &str| {
        gems["unlocated"]
            .as_array()
            .unwrap_or_else(|| panic!("{gems}"))
            .iter()
            .find(|u| u["gem"] == gem)
            .unwrap_or_else(|| panic!("{gem} is said: {gems}"))["why"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(why("other").contains("which revision"), "{gems}");
    assert!(why("gone").contains("no checkout of gone"), "{gems}");
    let kit = json(&trekr_env(
        &db,
        &dir,
        &["--def", "use.rb:1:1", "--json"],
        &env,
    ));
    assert!(
        kit["definition"][0]["root"]
            .as_str()
            .is_some_and(|root| root.ends_with("kit-0123456789ab")),
        "{kit}"
    );

    // Once a lockfile governs, an edit to the Gemfile waits for it.
    fs::write(dir.join("Gemfile.lock"), "GEM\n  specs:\n\nDEPENDENCIES\n").unwrap();
    let gemfile = fs::File::options()
        .append(true)
        .open(dir.join("Gemfile"))
        .unwrap();
    gemfile
        .set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(60))
        .unwrap();
    let text = stdout(&trekr_env(&db, &dir, &["--index"], &env));
    assert!(text.contains("newer than Gemfile.lock"), "{text}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// A stdlib method the core stub also writes keeps the stub's return type:
/// `Set#size` is real source in a Ruby where `Set` is not core (DEC-182).
#[test]
fn a_stdlib_method_the_core_stub_types_keeps_its_return() {
    let (dir, db) = scratch("stdlib-sig");
    let (home, _) = scratch("stdlib-sig-home");
    fake_ruby(
        &home,
        "9.8.7",
        &[(
            "set.rb",
            "class Set\n  def size\n    @hash.size\n  end\nend\n",
        )],
        &[],
    );
    ruby_app(&dir, "9.8.7", &[], &[]);
    fs::write(
        dir.join("job.rb"),
        "class Job\n  def run\n    Set.new.size.even?\n  end\nend\n",
    )
    .unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    trekr_env(&db, &dir, &["--index"], &env);

    let size = json(&trekr_env(
        &db,
        &dir,
        &["--def", "job.rb:3:13", "--json"],
        &env,
    ));
    assert!(
        size["definition"][0]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("set.rb")),
        "the real source is the location: {size}"
    );
    let even = json(&trekr_env(
        &db,
        &dir,
        &["--def", "job.rb:3:18", "--json"],
        &env,
    ));
    assert_eq!(even["status"], "resolved", "{even}");
    assert_eq!(even["owner"], "Integer", "{even}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// A gem reopening a stdlib class runs after it, so its method answers —
/// even when the stdlib was indexed after the gem (DEC-180).
#[test]
fn a_gem_reopening_the_stdlib_answers_over_it_whatever_the_index_order() {
    let (dir, db) = scratch("stdlib-layer");
    let (home, _) = scratch("stdlib-layer-home");
    fake_ruby(
        &home,
        "9.8.7",
        &[("stash.rb", "class Stash\n  def put(item)\n  end\nend\n")],
        &[],
    );
    ruby_app(
        &dir,
        "9.8.7",
        &["patcher (1.0.0)"],
        &[(
            "patcher-1.0.0",
            "patcher.rb",
            "class Stash\n  def put(item)\n    super\n  end\nend\n",
        )],
    );
    // No Ruby to be found yet: the gem is indexed first.
    fs::remove_file(dir.join(".ruby-version")).unwrap();
    let (nowhere, _) = scratch("stdlib-layer-nowhere");
    trekr_env(
        &db,
        &dir,
        &["--index"],
        &[("HOME", nowhere.to_str().unwrap())],
    );
    let env = [("HOME", home.to_str().unwrap())];
    fs::write(dir.join(".ruby-version"), "9.8.7\n").unwrap();
    let index = json(&trekr_env(&db, &dir, &["--index", "--json"], &env));
    assert_eq!(index["gems"]["stdlib"]["indexed"], true, "{index}");

    let answer = json(&trekr_env(&db, &dir, &["Stash#put", "--json"], &env));
    let roots = definition_roots(&answer);
    assert!(
        roots.len() == 1 && roots[0].ends_with("patcher-1.0.0"),
        "{answer}"
    );

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// Ruby loads the bundle before the app, so where both reopen a class the
/// app's method is the one that runs. The app is indexed first, and layering
/// by insert order let the gem's win.
#[test]
fn the_apps_reopen_answers_over_a_gems() {
    let (dir, db) = scratch("gem-layer");
    ruby_app(
        &dir,
        "9.8.7",
        &["patcher (1.0.0)"],
        &[(
            "patcher-1.0.0",
            "patcher.rb",
            "class Stash\n  def put(item)\n  end\nend\n",
        )],
    );
    // As a real app has it: the bundle is not the app's own code.
    fs::write(dir.join(".gitignore"), "vendor/\n").unwrap();
    fs::create_dir_all(dir.join("lib")).unwrap();
    fs::write(
        dir.join("lib/stash.rb"),
        "class Stash\n  def put(item)\n  end\nend\n\nStash.new.put(1)\n",
    )
    .unwrap();
    trekr(&db, &dir, &["--index"]);

    let answer = json(&trekr(&db, &dir, &["Stash#put", "--json"]));
    let roots = definition_roots(&answer);
    assert!(
        roots.len() == 1 && Path::new(&roots[0]) == fs::canonicalize(&dir).unwrap(),
        "{answer}"
    );
    let call = json(&trekr(&db, &dir, &["--def", "lib/stash.rb:6:11", "--json"]));
    assert_eq!(call["status"], "resolved", "{call}");
    assert!(
        call["definition"][0]["path"]
            .as_str()
            .is_some_and(|p| p.ends_with("lib/stash.rb")),
        "{call}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// A stdlib class whose Ruby loads a compiled extension has methods no Ruby
/// defines, so a name its Ruby lacks is residue there, and certain only for
/// a class that is all Ruby (DEC-181).
#[test]
fn a_stdlib_class_backed_by_a_compiled_extension_hedges_what_its_ruby_lacks() {
    let (dir, db) = scratch("stdlib-compiled");
    let (home, _) = scratch("stdlib-compiled-home");
    fake_ruby(
        &home,
        "9.8.7",
        &[
            (
                "gadget.rb",
                "require 'gadget.so'\nclass Gadget\n  def polish\n  end\nend\n",
            ),
            ("plain.rb", "class Plain\n  def polish\n  end\nend\n"),
            // Under the extension's directory, reopening core: `Object` is
            // compiled into Ruby, not into the extension.
            (
                "gadget/core_ext.rb",
                "class Object\n  def to_gadget\n  end\nend\n",
            ),
            ("arm64-test/rbconfig.rb", "module RbConfig\nend\n"),
            ("arm64-test/gadget.bundle", ""),
        ],
        &[],
    );
    ruby_app(&dir, "9.8.7", &[], &[]);
    let env = [("HOME", home.to_str().unwrap())];
    trekr_env(&db, &dir, &["--index"], &env);

    let compiled = json(&trekr_env(&db, &dir, &["Gadget#spin", "--json"], &env));
    assert_eq!(compiled["status"], "residue", "{compiled}");
    assert!(
        compiled["reason"].as_str().unwrap().contains("gadget"),
        "{compiled}"
    );
    // Plain's chain holds Object, which the extension's directory reopens.
    let plain = json(&trekr_env(&db, &dir, &["Plain#spin", "--json"], &env));
    assert_eq!(plain["status"], "no_such_method", "{plain}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// A block handed to a stdlib method runs where it is written, as one handed
/// to core does: `Dir.mktmpdir do … end` in an example still runs `expect` on
/// the example (DEC-180).
#[test]
fn a_block_handed_to_a_stdlib_method_runs_on_the_example() {
    let (dir, db) = scratch("stdlib-block");
    let (home, _) = scratch("stdlib-block-home");
    fake_ruby(
        &home,
        "9.8.7",
        &[(
            "tmpdir.rb",
            "class Dir\n  def self.mktmpdir(prefix = nil)\n    yield prefix\n  end\nend\n",
        )],
        &[],
    );
    ruby_app(&dir, "9.8.7", &[], &[]);
    let group = dir.join("rspec-core/lib/rspec/core/example_group.rb");
    fs::create_dir_all(group.parent().unwrap()).unwrap();
    fs::write(
        group,
        "module RSpec\n  module Core\n    class ExampleGroup\n      def expect(value)\n      \
         end\n    end\n  end\nend\n",
    )
    .unwrap();
    fs::create_dir_all(dir.join("spec")).unwrap();
    fs::write(
        dir.join("spec/widget_spec.rb"),
        "RSpec.describe \"Widget\" do\n  it \"works\" do\n    Dir.mktmpdir do |path|\n      \
         expect(path)\n    end\n  end\nend\n",
    )
    .unwrap();
    let env = [("HOME", home.to_str().unwrap())];
    trekr_env(&db, &dir, &["--index"], &env);

    let answer = json(&trekr_env(
        &db,
        &dir,
        &["--def", "spec/widget_spec.rb:4:7", "--json"],
        &env,
    ));
    assert_eq!(answer["status"], "resolved", "{answer}");
    assert_eq!(answer["owner"], "RSpec::Core::ExampleGroup", "{answer}");

    let _ = fs::remove_dir_all(&dir);
    let _ = fs::remove_dir_all(&home);
}

/// Mark `dir`'s checkout as an index still filling the store (DEC-320), as
/// the process `pid` would, with `read` of `of` files in.
fn mark_warming(db: &Path, dir: &Path, pid: u32, read: u64, of: u64) {
    let root = fs::canonicalize(dir).unwrap();
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
            [
                format!("warming {}", root.to_string_lossy()),
                format!("{pid} {read} {of}"),
            ],
        )
        .unwrap();
}

/// An answer from an index still being filled says so, and claims nothing
/// a partial index cannot back: no certain absence, no ruled-out caller, no
/// dead code. Exit 2 is "no answer yet" for a miss, as for `not_indexed`.
#[test]
fn a_partial_index_says_so_and_claims_nothing_certain() {
    let (dir, db) = scratch("warming");
    repo(&dir);
    fs::write(
        dir.join("user.rb"),
        "class Gadget\n  def resize(a)\n  end\nend\n\nclass User\n  def go\n    \
         Widget.new.resize(1, 2)\n    Gadget.new.resize(1)\n  end\nend\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    trekr(&db, &dir, &["--index"]);

    // Whole: the call on a Gadget is ruled out, a missing method is certain.
    let refs = json(&trekr(&db, &dir, &["--refs", "Widget#resize", "--json"]));
    assert_eq!(refs["counts"]["excluded"], 1);
    assert!(refs.get("warming").is_none());
    let missing = trekr(&db, &dir, &["--refs", "Gadget#nope", "--json"]);
    assert_eq!(json(&missing)["status"], "no_such_method");
    assert_eq!(missing.status.code(), Some(1));

    // An index under way, a quarter read.
    mark_warming(&db, &dir, std::process::id(), 1, 4);
    let def = trekr(&db, &dir, &["--def", "user.rb:8:5", "--json"]);
    let answer = json(&def);
    assert_eq!(answer["status"], "resolved");
    assert_eq!(answer["warming"]["read"], 1);
    assert_eq!(answer["warming"]["of"], 4);
    assert_eq!(answer["warming"]["interrupted"], false);
    assert_eq!(answer["confidence"], 0.25, "scaled by the share read");
    assert_eq!(def.status.code(), Some(0), "an answer is still an answer");

    let text = trekr(&db, &dir, &["--def", "user.rb:8:5"]);
    assert!(String::from_utf8_lossy(&text.stderr).contains("still being indexed — 1 of 4 files"));

    let residue = trekr(
        &db,
        &dir,
        &["--def", "widget.rb:1:17", "--json", "--no-index"],
    );
    assert_eq!(json(&residue)["status"], "residue");
    assert_eq!(residue.status.code(), Some(2), "a miss is no answer yet");

    // Asked not to wait for the rest, a whole-checkout question answers
    // from the part read.
    let refs = json(&trekr(
        &db,
        &dir,
        &["--refs", "Widget#resize", "--json", "--no-index"],
    ));
    assert_eq!(refs["counts"]["excluded"], 0, "nothing ruled out");
    let gadget = refs["references"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["line"] == 9)
        .expect("the Gadget call is listed, as possible");
    assert_eq!(gadget["tier"], "possible");
    assert!(refs["warming"].is_object());

    let missing = trekr(
        &db,
        &dir,
        &["--refs", "Gadget#nope", "--json", "--no-index"],
    );
    assert_eq!(
        json(&missing)["status"],
        "residue",
        "absence is not certain"
    );
    assert_eq!(missing.status.code(), Some(2));

    let dead = trekr(&db, &dir, &["--dead", ".", "--json", "--no-index"]);
    assert_eq!(json(&dead)["status"], "warming");
    assert_eq!(dead.status.code(), Some(2));

    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert_eq!(status["checkouts"][0]["warming"]["read"], 1, "{status}");
    let text = stdout(&trekr(&db, &dir, &["--status"]));
    assert!(text.contains("being indexed: 1 of 4 files"), "{text}");

    // The index that marked it died: still partial, and says what fixes it.
    mark_warming(&db, &dir, i32::MAX as u32, 1, 4);
    let def = json(&trekr(
        &db,
        &dir,
        &["--def", "user.rb:8:5", "--json", "--no-index"],
    ));
    assert_eq!(def["warming"]["interrupted"], true);
    assert!(
        def["warming"]["hint"]
            .as_str()
            .unwrap()
            .contains("trekr --index")
    );
    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert_eq!(
        status["checkouts"][0]["warming"], def["warming"],
        "{status}"
    );
    let text = stdout(&trekr(&db, &dir, &["--status"]));
    assert!(
        text.contains("cut short: 1 of 4 files read — `trekr --index"),
        "{text}"
    );

    // And an index ends it.
    trekr(&db, &dir, &["--index"]);
    let def = json(&trekr(&db, &dir, &["--def", "user.rb:8:5", "--json"]));
    assert!(def.get("warming").is_none());
    assert_eq!(def["confidence"], 1.0);
    let status = json(&trekr(&db, &dir, &["--status", "--json"]));
    assert!(status["checkouts"][0].get("warming").is_none(), "{status}");
}

/// An index the language server spawns reads the files it is told are open
/// first (DEC-322), and ends with the store and report an index told nothing
/// leaves, its mark cleared.
#[test]
fn a_first_index_told_what_is_open_ends_where_one_told_nothing_does() {
    use std::io::Write;
    let (dir, db) = scratch("hinted");
    repo(&dir);
    fs::create_dir_all(dir.join("app/models")).unwrap();
    fs::write(
        dir.join("app/models/order.rb"),
        "class Order\n  def go\n    Widget.new.resize(1)\n  end\nend\n",
    )
    .unwrap();
    git(&dir, &["add", "-A"]);
    let plain = json(&trekr(&db, &dir, &["--index", "--json"]));

    let (_, hinted_db) = scratch("hinted-told");
    let mut child = neutral(Command::new(env!("CARGO_BIN_EXE_trekr")))
        .args(["--index", "--json"])
        .current_dir(&dir)
        .env("TREKR_DB", &hinted_db)
        .env("TREKR_BACKGROUND", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let open = fs::canonicalize(dir.join("app/models/order.rb")).unwrap();
    writeln!(child.stdin.take().unwrap(), "{}", open.display()).unwrap();
    let hinted = child.wait_with_output().unwrap();
    assert!(hinted.status.success());
    let hinted = json(&hinted);
    assert_eq!(hinted["indexed"], plain["indexed"]);

    let def = json(&trekr(
        &hinted_db,
        &dir,
        &["--def", "app/models/order.rb:3:5", "--json"],
    ));
    assert_eq!(def["status"], "resolved");
    assert!(def.get("warming").is_none(), "the mark is gone: {def}");
}

/// A gem calls an app's method by its name on an object it was handed —
/// simple_form's `@object.type_for_attribute`, http's `@socket.reset_counter`
/// — and no app call site writes it, so `--dead` called such methods
/// unreferenced, clear (DEC-367). It needs the app and its gem, so it lives
/// here rather than in the testbed.
#[test]
fn dead_says_a_gem_in_the_bundle_calls_the_name() {
    let (app, db) = scratch("dead-bundle-app");
    let (gems, _) = scratch("dead-bundle-gems");
    let lib = gems.join("gems/former-1.0.0/lib");
    fs::create_dir_all(&lib).unwrap();
    fs::write(
        lib.join("former.rb"),
        "class Former\n  def kind_of(object, name)\n    \
         object.type_for_attribute(name) if object.respond_to?(:type_for_attribute)\n  \
         end\nend\n",
    )
    .unwrap();
    git(&app, &["init", "-q"]);
    fs::write(
        app.join("widget.rb"),
        "class Widget\n  def type_for_attribute(name)\n    name\n  end\n\n  \
         def self.type_for_attribute(name)\n    name\n  end\n\n  \
         def lonely\n    :lonely\n  end\nend\n",
    )
    .unwrap();
    fs::write(
        app.join("Gemfile.lock"),
        "GEM\n  remote: https://rubygems.org/\n  specs:\n    former (1.0.0)\n\
         \nPLATFORMS\n  ruby\n\nDEPENDENCIES\n  former\n",
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
    assert!(out.status.success(), "indexed the app and its gem");

    let dead = json(&trekr(&db, &app, &["--dead", "widget.rb", "--json"]));
    let row = |name: &str, singleton: bool| {
        dead["candidates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["name"] == name && r["singleton"] == singleton)
            .cloned()
            .unwrap_or_else(|| panic!("{name} listed: {dead}"))
    };
    let hooked = row("type_for_attribute", false);
    assert_eq!(hooked["tier"], "unreferenced", "{hooked}");
    assert_eq!(hooked["confidence"], "lower", "{hooked}");
    assert!(
        hooked["caveat"].as_str().unwrap().contains("former-1.0.0"),
        "names the gem: {hooked}"
    );
    // The gem's call is on an instance: it is no evidence for the class side.
    assert_eq!(row("type_for_attribute", true)["confidence"], "clear");
    assert_eq!(row("lonely", false)["confidence"], "clear");
}

/// `--dead` and `--refs FILE:LINE:COL` ask the same reader about an example
/// group's member, so they cannot disagree: every member `--dead` lists has
/// no read `--refs` would show, and every member it does not list (bar a
/// `let!`, and the members of a shared group it lists) has one (DEC-490).
/// Run over the testbed's example group cases, staged as one checkout.
#[test]
fn dead_and_refs_agree_on_an_example_groups_members() {
    let (dir, db) = scratch("members-agree");
    git(&dir, &["init", "-q"]);
    let testbed = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testbed");
    let mut cases = 0;
    for entry in fs::read_dir(&testbed).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let numbered: u32 = name.split('-').next().unwrap().parse().unwrap_or(0);
        let members = (490..=499).contains(&numbered) || (604..=606).contains(&numbered);
        if !members || !entry.path().join("spec").is_dir() {
            continue;
        }
        let into = dir.join(&name);
        fs::create_dir_all(&into).unwrap();
        let copied = Command::new("cp")
            .arg("-R")
            .arg(entry.path().join("spec"))
            .arg(&into)
            .status()
            .unwrap();
        assert!(copied.success());
        cases += 1;
    }
    assert!(cases >= 5, "the testbed's example group cases were staged");
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
    assert!(trekr(&db, &dir, &["--index"]).status.success());

    let dead = json(&trekr(&db, &dir, &["--dead", ".", "--json"]));
    let listed: Vec<(String, u64, u64)> = dead["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| {
            matches!(row["kind"].as_str(), Some("let" | "subject")) || row["group"].is_string()
        })
        .map(|row| {
            let path = row["path"].as_str().unwrap();
            let relative = path
                .strip_prefix(&format!("{}/", dir.display()))
                .unwrap_or(path);
            (
                relative.to_string(),
                row["line"].as_u64().unwrap(),
                row["col"].as_u64().unwrap(),
            )
        })
        .collect();
    assert!(!listed.is_empty(), "the cases list members: {dead}");
    let reads = |path: &str, line: u64, col: u64| {
        let at = format!("{path}:{line}:{col}");
        let answer = json(&trekr(&db, &dir, &["--refs", &at, "--json"]));
        let counts = &answer["counts"];
        assert!(
            counts.is_object(),
            "--refs {at} answers for a member: {answer}"
        );
        counts["confirmed"].as_u64().unwrap() + counts["possible"].as_u64().unwrap()
    };
    for (path, line, col) in &listed {
        assert_eq!(
            reads(path, *line, *col),
            0,
            "--dead lists {path}:{line}, so --refs finds no read"
        );
    }

    // Every other `let` and `subject` the cases write is read, but for the
    // members of a shared group `--dead` lists, which go with it.
    let unincluded: Vec<String> = dead["candidates"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|row| row["kind"] == "shared_group")
        .map(|row| format!("::{}", row["owner"].as_str().unwrap()))
        .collect();
    let symbols = |path: &str| json(&trekr(&db, &dir, &["--symbols", path, "--json"]));
    for (path, _, _) in &listed {
        let outline = symbols(path);
        for symbol in outline.as_array().into_iter().flatten() {
            let line = symbol["line"].as_u64().unwrap();
            let col = symbol["col"].as_u64().unwrap_or(0);
            let via = symbol["via"].as_str().unwrap_or_default();
            let in_unincluded = symbol["nesting"][0]
                .as_str()
                .is_some_and(|scope| unincluded.iter().any(|module| module == scope));
            if !matches!(via, "let" | "subject")
                || in_unincluded
                || listed.iter().any(|(p, l, _)| p == path && *l == line)
            {
                continue;
            }
            assert!(
                reads(path, line, col) > 0,
                "--dead does not list {path}:{line}, so --refs finds a read"
            );
        }
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn a_store_that_cannot_be_opened_is_named_in_the_error() {
    let (dir, _) = scratch("unopenable");
    // A regular file where the store's directory would be made.
    let blocker = dir.join("blocker");
    fs::write(&blocker, "").unwrap();
    let db = blocker.join("store/trekr.db");
    let out = trekr(&db, &dir, &["--status", "--json"]);
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains(&db.display().to_string()), "{stderr}");

    // A directory it cannot write: SQLite's reason, and the path, once each.
    let locked = dir.join("locked");
    fs::create_dir_all(&locked).unwrap();
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o555)).unwrap();
    let db = locked.join("trekr.db");
    let out = trekr(&db, &dir, &["--status"]);
    assert_eq!(out.status.code(), Some(74));
    let stderr = String::from_utf8_lossy(&out.stderr);
    let path = db.display().to_string();
    assert_eq!(stderr.matches(&path).count(), 1, "{stderr}");
    assert_eq!(stderr.matches("unable to open").count(), 1, "{stderr}");
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    let _ = fs::remove_dir_all(&dir);
}
