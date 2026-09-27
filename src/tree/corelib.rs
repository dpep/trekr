//! Ruby core, served as one file per top-level class or module.
//!
//! `core.rb` stays the single source (DEC-015). What a definition points at is
//! a file named for its owner — `<core>/String.rb` — because an editor's peek
//! list shows a file name and the target's first line, and several hits in one
//! `core.rb` read as copies of the same thing (DEC-078). Each file is
//! extracted as it is served, so its lines are the file's own.

use std::path::{Path, PathBuf};

/// What every core site's path starts with. Deliberately not a real path: a
/// file is written for it only when something has to open one.
pub(crate) const CORE_PATH: &str = "<core>";

/// One owner's stub, as a caller sees it.
pub(crate) struct CoreFile {
    /// `String.rb`.
    pub(crate) name: String,
    pub(crate) text: String,
}

impl CoreFile {
    /// The site path its definitions carry: `<core>/String.rb`.
    pub(crate) fn site_path(&self) -> String {
        format!("{CORE_PATH}/{}", self.name)
    }
}

/// Is this site in Ruby core?
pub(crate) fn is_core(path: &str) -> bool {
    path.strip_prefix(CORE_PATH)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Every core file, split from `core.rb` once per process.
pub(crate) fn files() -> &'static [CoreFile] {
    static FILES: std::sync::OnceLock<Vec<CoreFile>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| split(include_str!("core.rb")))
}

/// `core.rb` cut at its top-level `class`/`module` blocks.
///
/// A block takes the comment written directly above it. Code outside any
/// block — the top-level constants — goes to `Object.rb`, since a top-level
/// constant is Object's. `core.rb`'s own header describes the whole corpus and
/// is left behind; each file gets a line of its own saying what it is.
fn split(source: &str) -> Vec<CoreFile> {
    let mut blocks: Vec<(String, Vec<&str>)> = Vec::new();
    let mut loose: Vec<&str> = Vec::new();
    let mut comment: Vec<&str> = Vec::new();
    let mut open: Option<(String, Vec<&str>)> = None;
    let mut seen_code = false;
    for line in source.lines() {
        if let Some((_, lines)) = open.as_mut() {
            lines.push(line);
            if line == "end" {
                blocks.push(open.take().expect("a block is open"));
            }
            continue;
        }
        if line.starts_with('#') {
            // The header is everything before the first code; after that a
            // comment belongs to what follows it.
            if seen_code {
                comment.push(line);
            }
            continue;
        }
        if line.trim().is_empty() {
            comment.clear();
            continue;
        }
        seen_code = true;
        let mut lines = std::mem::take(&mut comment);
        lines.push(line);
        match top_level_name(line) {
            Some(name) if line.ends_with("; end") => blocks.push((name, lines)),
            Some(name) => open = Some((name, lines)),
            None => loose.extend(lines),
        }
    }
    debug_assert!(open.is_none(), "core.rb ends inside a block");

    let mut files: Vec<CoreFile> = Vec::new();
    for (name, lines) in blocks {
        debug_assert!(
            !files.iter().any(|f| f.name == format!("{name}.rb")),
            "core.rb declares {name} twice; one file cannot hold both"
        );
        let mut text = header(&name);
        text.push_str(&lines.join("\n"));
        text.push('\n');
        if name == "Object" && !loose.is_empty() {
            text.push('\n');
            text.push_str(&loose.join("\n"));
            text.push('\n');
        }
        files.push(CoreFile {
            name: format!("{name}.rb"),
            text,
        });
    }
    files
}

fn header(name: &str) -> String {
    format!(
        "# Ruby core: {name}. A stub trekr navigates by, not Ruby's source;\n\
         # the real documentation is https://docs.ruby-lang.org/en/3.4/{name}.html\n\n"
    )
}

/// `String` from `class String < Object`, `Kernel` from `module Kernel`.
fn top_level_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("class ")
        .or_else(|| line.strip_prefix("module "))?;
    let name: String = rest
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// Write the core files into `dir`, rewriting only what differs so an editor
/// watching them is not churned. Once per directory per process.
pub(crate) fn materialize(dir: &Path) -> std::io::Result<()> {
    static DONE: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());
    let mut done = DONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if done.iter().any(|d| d == dir) {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    for file in files() {
        let path = dir.join(&file.name);
        if std::fs::read_to_string(&path).ok().as_deref() != Some(file.text.as_str()) {
            std::fs::write(&path, &file.text)?;
        }
    }
    done.push(dir.to_path_buf());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file<'a>(files: &'a [CoreFile], name: &str) -> &'a CoreFile {
        files
            .iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("no {name}"))
    }

    #[test]
    fn each_top_level_owner_is_its_own_file() {
        let files = split(
            "# header\n\nclass A < Object\n  def x\n  end\nend\n\n# about B\nmodule B\nend\nclass C < A; end\n",
        );
        let names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["A.rb", "B.rb", "C.rb"]);
        assert!(
            file(&files, "B.rb")
                .text
                .contains("# about B\nmodule B\nend\n")
        );
        assert!(!file(&files, "A.rb").text.contains("# header"));
    }

    #[test]
    fn top_level_constants_go_to_object() {
        let files = split("class Object\nend\n\nENV = nil\n");
        assert!(
            file(&files, "Object.rb")
                .text
                .ends_with("end\n\nENV = nil\n")
        );
    }

    #[test]
    fn the_real_corpus_splits_into_distinct_valid_files() {
        let files = files();
        let mut names: Vec<&str> = files.iter().map(|f| f.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), files.len(), "a name declared twice");
        for f in files {
            let facts = crate::extract::extract(f.text.as_bytes());
            assert_eq!(facts.parse_errors, 0, "{} must be valid Ruby", f.name);
        }
        assert!(
            file(files, "String.rb")
                .text
                .contains("  def downcase(*options)\n  end\n")
        );
    }

    #[test]
    fn core_paths_are_recognised_and_nothing_else_is() {
        assert!(is_core("<core>/String.rb"));
        assert!(is_core("<core>"));
        assert!(!is_core("<corelib>/x.rb"));
        assert!(!is_core("lib/core.rb"));
    }
}
