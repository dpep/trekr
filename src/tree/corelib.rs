//! Ruby core, served as one file per top-level class or module.
//!
//! `core.rb` stays the single source (DEC-015). What a definition points at is
//! a file named for its owner — `<core>/String.rb` — because an editor's peek
//! list shows a file name and the target's first line, and several hits in one
//! `core.rb` read as copies of the same thing (DEC-078). Each file is
//! extracted as it is served, so its lines are the file's own.

use std::collections::HashMap;
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

/// RSpec's runtime wiring, stated as source and served beside core (DEC-087).
/// Its methods are declarations: RSpec makes them when a suite boots.
pub(crate) const RSPEC_STUB: &str = "<core>/RSpec.rb";

/// The RSpec stub, as a caller sees it.
pub(crate) fn rspec_file() -> &'static CoreFile {
    static FILE: std::sync::OnceLock<CoreFile> = std::sync::OnceLock::new();
    FILE.get_or_init(|| CoreFile {
        name: "RSpec.rb".to_string(),
        text: include_str!("rspec.rb").to_string(),
    })
}

/// Where the stdlib's compiled half is served: `<core>/stdlib/Pathname.rb`
/// (DEC-220). Under core, since what is compiled into a Ruby is Ruby's own.
const STDLIB_DIR: &str = "stdlib/";

/// The stdlib's compiled half, one file per top-level owner, as a caller
/// sees it (DEC-220). Served only for a checkout whose stdlib is indexed.
pub(crate) fn stdlib_files() -> &'static [CoreFile] {
    static FILES: std::sync::OnceLock<Vec<CoreFile>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| split(include_str!("stdlib.rb"), STDLIB_DIR, stdlib_header))
}

/// Return types for the stdlib's Ruby methods, which lend them to the real
/// definitions and are never a location (DEC-220). Its path is never opened.
pub(crate) const STDLIB_SIGS: &str = "<core>/stdlib-sigs.rb";

/// One `def` of a generated stub, cut out with what it needs to extract to
/// the same method, so that a query parses only the names it asks about.
/// Parsing both stubs whole cost every tree build ~12 ms, twice core's.
pub(crate) struct StubDef {
    pub(crate) owner: String,
    pub(crate) singleton: bool,
    pub(crate) name: String,
    /// The file its site is in.
    pub(crate) path: String,
    /// Its `def`'s line in that file, less its line in `source`.
    pub(crate) shift: u32,
    /// Its owner, visibility, `sig`s and `def`, as Ruby.
    pub(crate) source: String,
}

/// The stdlib's compiled methods, by name.
pub(crate) fn stdlib_defs() -> &'static HashMap<String, Vec<StubDef>> {
    static DEFS: std::sync::OnceLock<HashMap<String, Vec<StubDef>>> = std::sync::OnceLock::new();
    DEFS.get_or_init(|| {
        let mut by_name: HashMap<String, Vec<StubDef>> = HashMap::new();
        for file in stdlib_files() {
            for def in cut(&file.site_path(), &file.text) {
                by_name.entry(def.name.clone()).or_default().push(def);
            }
        }
        by_name
    })
}

/// The return types lent to the stdlib's Ruby methods, by (owner,
/// singleton, name).
pub(crate) fn stdlib_sig_defs() -> &'static HashMap<(String, bool, String), StubDef> {
    static DEFS: std::sync::OnceLock<HashMap<(String, bool, String), StubDef>> =
        std::sync::OnceLock::new();
    DEFS.get_or_init(|| {
        cut(STDLIB_SIGS, stdlib_sigs())
            .into_iter()
            .map(|def| ((def.owner.clone(), def.singleton, def.name.clone()), def))
            .collect()
    })
}

fn stdlib_sigs() -> &'static str {
    include_str!("stdlib_sigs.rb")
}

/// A generated stub's `def`s, each with its owner written compactly around
/// it. Reads only the shape `script/stdlib_sigs.rb` writes — nested
/// `class`/`module` blocks, `private`/`protected` lines, `sig`s directly
/// above each `def` — and leaves the Ruby itself to the extractor.
fn cut(path: &str, text: &str) -> Vec<StubDef> {
    let mut defs = Vec::new();
    // (indent, owner, visibility) for each open block.
    let mut owners: Vec<(usize, String, &str)> = Vec::new();
    let mut sigs: Vec<&str> = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        let opens = trimmed
            .strip_prefix("class ")
            .or_else(|| trimmed.strip_prefix("module "));
        if let Some(rest) = opens.filter(|_| !trimmed.ends_with("; end")) {
            let name: String = rest
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == ':')
                .collect();
            let owner = match owners.last() {
                Some((_, outer, _)) => format!("{outer}::{name}"),
                None => name,
            };
            owners.push((indent, owner, "public"));
        } else if trimmed == "end" {
            if owners.last().is_some_and(|(open, _, _)| *open == indent) {
                owners.pop();
            }
        } else if let Some(visibility) = ["private", "protected", "public"]
            .into_iter()
            .find(|v| trimmed == *v)
        {
            if let Some(owner) = owners.last_mut() {
                owner.2 = visibility;
            }
        } else if trimmed.starts_with("sig {") {
            sigs.push(line);
        } else if let Some(rest) = trimmed.strip_prefix("def ") {
            let Some((_, owner, visibility)) = owners.last() else {
                continue;
            };
            let (singleton, rest) = match rest.strip_prefix("self.") {
                Some(rest) => (true, rest),
                None => (false, rest),
            };
            let name = rest.split('(').next().unwrap_or(rest).trim().to_string();
            let mut source = format!("class {owner}\n");
            if *visibility != "public" {
                source.push_str(visibility);
                source.push('\n');
            }
            for sig in sigs.drain(..) {
                source.push_str(sig);
                source.push('\n');
            }
            let def_line = source.lines().count() as u32 + 1;
            source.push_str(line);
            source.push_str(&format!("\n{}end\nend\n", " ".repeat(indent)));
            defs.push(StubDef {
                owner: owner.clone(),
                singleton,
                name,
                path: path.to_string(),
                shift: at as u32 + 1 - def_line,
                source,
            });
        }
    }
    defs
}

/// Is this site a declaration in the stdlib's compiled half?
pub(crate) fn is_stdlib_stub(path: &str) -> bool {
    path.strip_prefix(CORE_PATH)
        .and_then(|rest| rest.strip_prefix('/'))
        .is_some_and(|rest| rest.starts_with(STDLIB_DIR))
}

/// Is this site in the RSpec stub?
pub(crate) fn is_rspec_stub(path: &str) -> bool {
    path == RSPEC_STUB
}

/// Is this site in Ruby core?
pub(crate) fn is_core(path: &str) -> bool {
    path.strip_prefix(CORE_PATH)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Every core file, split from `core.rb` once per process.
pub(crate) fn files() -> &'static [CoreFile] {
    static FILES: std::sync::OnceLock<Vec<CoreFile>> = std::sync::OnceLock::new();
    FILES.get_or_init(|| split(include_str!("core.rb"), "", header))
}

/// A stub cut at its top-level `class`/`module` blocks, each file named for
/// its owner under `dir`.
///
/// A block takes the comment written directly above it. Code outside any
/// block — the top-level constants — goes to `Object.rb`, since a top-level
/// constant is Object's. `core.rb`'s own header describes the whole corpus and
/// is left behind; each file gets a line of its own saying what it is.
fn split(source: &str, dir: &str, header: fn(&str) -> String) -> Vec<CoreFile> {
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
            !files.iter().any(|f| f.name == format!("{dir}{name}.rb")),
            "{name} is declared twice; one file cannot hold both"
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
            name: format!("{dir}{name}.rb"),
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

fn stdlib_header(name: &str) -> String {
    format!(
        "# Ruby's stdlib: {name}, the methods compiled into it rather than\n\
         # written in Ruby, from their RBS signatures. A stub trekr navigates by;\n\
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
    std::fs::create_dir_all(dir.join(STDLIB_DIR))?;
    for file in files().iter().chain(stdlib_files()).chain([rspec_file()]) {
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
            "",
            header,
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
        let files = split("class Object\nend\n\nENV = nil\n", "", header);
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
    fn the_stdlib_stubs_are_valid_ruby_served_under_their_own_directory() {
        for f in stdlib_files() {
            let facts = crate::extract::extract(f.text.as_bytes());
            assert_eq!(facts.parse_errors, 0, "{} must be valid Ruby", f.name);
            assert!(is_stdlib_stub(&f.site_path()), "{}", f.name);
        }
        let facts = crate::extract::extract(stdlib_sigs().as_bytes());
        assert_eq!(facts.parse_errors, 0);
        assert!(stdlib_defs().contains_key("hexdigest"));
        assert!(
            file(stdlib_files(), "stdlib/Pathname.rb")
                .text
                .contains("  def read(")
        );
        assert!(!is_stdlib_stub("<core>/String.rb"));
        assert!(is_core(STDLIB_SIGS) && !is_stdlib_stub(STDLIB_SIGS));
    }

    #[test]
    fn the_rspec_stub_is_valid_ruby_that_declares_no_class_of_its_own() {
        let facts = crate::extract::extract(rspec_file().text.as_bytes());
        assert_eq!(facts.parse_errors, 0);
        assert!(is_core(&format!("{CORE_PATH}/{}", rspec_file().name)));
        assert!(is_rspec_stub(RSPEC_STUB));
    }

    #[test]
    fn core_paths_are_recognised_and_nothing_else_is() {
        assert!(is_core("<core>/String.rb"));
        assert!(is_core("<core>"));
        assert!(!is_core("<corelib>/x.rb"));
        assert!(!is_core("lib/core.rb"));
    }
}
