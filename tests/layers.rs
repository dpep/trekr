//! The fronts sit on the engine, never the engine on a front, and neither
//! front on the other: the LSP does not borrow the CLI's query logic, and
//! resolve does not reach into the LSP for a helper. A leak compiles fine, so
//! this reads the source for one.

use std::path::{Path, PathBuf};

/// The fronts a module under `src/<top>` may not name.
fn forbidden(top: &str) -> &'static [&'static str] {
    match top {
        "cli" | "main.rs" | "lib.rs" => &[],
        "serve" => &["cli"],
        _ => &["cli", "serve"],
    }
}

fn rust_files(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("src is readable").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, found);
        } else if path.extension().is_some_and(|e| e == "rs") {
            found.push(path);
        }
    }
}

/// Does `line` name `crate::<front>`, directly or in a `use crate::{…}` group?
fn names(line: &str, front: &str) -> bool {
    let direct = format!("crate::{front}");
    let at_boundary = |text: &str, word: &str| {
        text.match_indices(word).any(|(at, _)| {
            !text[at + word.len()..].starts_with(|c: char| c.is_alphanumeric() || c == '_')
        })
    };
    at_boundary(line, &direct)
        || line.split("crate::{").skip(1).any(|group| {
            let group = group.split('}').next().unwrap_or_default();
            group
                .split(|c: char| !(c.is_alphanumeric() || c == '_'))
                .any(|word| word == front)
        })
}

#[test]
fn no_layer_imports_a_front_above_it() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let mut leaks = Vec::new();
    for file in &files {
        let relative = file.strip_prefix(&src).unwrap();
        let top = relative.components().next().unwrap().as_os_str();
        let fronts = forbidden(&top.to_string_lossy());
        let text = std::fs::read_to_string(file).unwrap();
        for (n, line) in text.lines().enumerate() {
            for front in fronts.iter().filter(|front| names(line, front)) {
                leaks.push(format!(
                    "{}:{} names crate::{front}",
                    relative.display(),
                    n + 1
                ));
            }
        }
    }
    assert!(leaks.is_empty(), "{}", leaks.join("\n"));
}

#[test]
fn a_front_is_found_however_it_is_imported() {
    assert!(names("use crate::cli::position;", "cli"));
    assert!(names(
        "    let x = crate::serve::vars::analyze(s);",
        "serve"
    ));
    assert!(names("use crate::{core, serve::fresh};", "serve"));
    assert!(!names("use crate::client::x;", "cli"));
    assert!(!names("use crate::{core, server};", "serve"));
}
