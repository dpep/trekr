//! Ruby core and the stdlib's compiled half, served as one file per
//! top-level class or module.
//!
//! The stubs are written at index time from the rbs gem the app's Ruby
//! carries (DEC-240) and stored with its stdlib; a tree serves the ones its
//! Ruby has. What a definition points at is a file named for its owner —
//! `<core>/<rbs>/String.rb` — because an editor's peek list shows a file name
//! and the target's first line, and several hits in one file read as copies
//! of the same thing (DEC-078). Each file is extracted as it is served, so its
//! lines are the file's own.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

/// What every core site's path starts with. Deliberately not a real path: a
/// file is written for it only when something has to open one.
pub(crate) const CORE_PATH: &str = "<core>";

/// One owner's stub, as a caller sees it.
pub(crate) struct CoreFile {
    /// `String.rb`, `stdlib/Pathname.rb`.
    pub(crate) name: String,
    pub(crate) text: String,
}

/// RSpec's runtime wiring, stated as source and served beside core (DEC-087).
/// Its methods are declarations: RSpec makes them when a suite boots.
pub(crate) const RSPEC_STUB: &str = "<core>/RSpec.rb";

/// The RSpec stub, as a caller sees it.
pub(crate) fn rspec_file() -> &'static CoreFile {
    static FILE: OnceLock<CoreFile> = OnceLock::new();
    FILE.get_or_init(|| CoreFile {
        name: "RSpec.rb".to_string(),
        text: include_str!("rspec.rb").to_string(),
    })
}

/// Where the stdlib's compiled half is served, under its Ruby's directory:
/// `<core>/<rbs>/stdlib/Pathname.rb` (DEC-220). Under core, since what is
/// compiled into a Ruby is Ruby's own.
const STDLIB_DIR: &str = "stdlib/";

/// The return types lent to the stdlib's Ruby methods, which are never a
/// location (DEC-220): `<core>/<rbs>/stdlib-sigs.rb`, never opened.
const STDLIB_SIGS: &str = "stdlib-sigs.rb";

/// One `def` of a stub, cut out with what it needs to extract to the same
/// method, so that a query parses only the names it asks about. Parsing the
/// stubs whole cost every tree build more than the rest of it (DEC-220).
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

/// One Ruby's stubs, from its signatures, each part cut on first use.
pub(crate) struct Stubs {
    /// `rbs-3.8.0-1a2b3c4d`: the directory its files are served under, one
    /// per Ruby and rbs gem, so two Rubies' `String.rb` never collide.
    pub(crate) id: String,
    pub(crate) version: String,
    core_text: String,
    stdlib_text: String,
    sigs_text: String,
    core: OnceLock<Vec<CoreFile>>,
    stdlib: OnceLock<Vec<CoreFile>>,
    core_defs: OnceLock<HashMap<String, Vec<StubDef>>>,
    stdlib_defs: OnceLock<HashMap<String, Vec<StubDef>>>,
    sig_defs: OnceLock<HashMap<(String, bool, String), StubDef>>,
}

/// `rbs-3.8.0-1a2b3c4d`: the directory one Ruby's stubs are written under.
pub(crate) fn dir_name(version: &str, key: &str) -> String {
    format!("rbs-{version}-{}", &key[..key.len().min(8)])
}

/// Remove from a store's core directory each Ruby's directory whose
/// signatures the store no longer holds (`live` names those it does).
pub(crate) fn sweep(
    dir: &Path,
    live: &std::collections::HashSet<String>,
    dry_run: bool,
) -> super::files::Swept {
    let mut swept = super::files::Swept::default();
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with("rbs-")
            && entry.file_type().is_ok_and(|t| t.is_dir())
            && !live.contains(name)
        {
            remove(&entry.path(), &mut swept, dry_run);
        }
    }
    swept
}

/// Remove what builds before a core directory per store wrote beside the
/// store at `beside`: `core.rb`, and in `core/` 0.8.0's flat `String.rb`…,
/// a later build's `stdlib/` and `rbs-*` directories, and `RSpec.rb` — the
/// directory itself once empty. Only a `core/` whose `RSpec.rb` is trekr's
/// is touched: the name is common, and the store may sit beside anything.
pub(crate) fn sweep_legacy(beside: &Path, dry_run: bool) -> super::files::Swept {
    let mut swept = super::files::Swept::default();
    let single = beside.join("core.rb");
    if std::fs::read_to_string(&single)
        .is_ok_and(|text| text.starts_with("# Ruby's core library, as far as navigation cares"))
    {
        remove(&single, &mut swept, dry_run);
    }
    let dir = beside.join("core");
    let ours = std::fs::read_to_string(dir.join(&rspec_file().name))
        .is_ok_and(|text| text.lines().next() == rspec_file().text.lines().next());
    if !ours {
        return swept;
    }
    for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
        let legacy = match is_dir {
            true => name == "stdlib" || name.starts_with("rbs-"),
            false => name.ends_with(".rb"),
        };
        if legacy {
            remove(&entry.path(), &mut swept, dry_run);
        }
    }
    if !dry_run {
        let _ = std::fs::remove_dir(&dir);
    }
    swept
}

fn remove(path: &Path, swept: &mut super::files::Swept, dry_run: bool) {
    let (files, bytes) = measure(path);
    swept.files += files;
    swept.bytes += bytes;
    if !dry_run {
        let _ = match path.is_dir() {
            true => std::fs::remove_dir_all(path),
            false => std::fs::remove_file(path),
        };
    }
}

/// Files and bytes under `path`.
fn measure(path: &Path) -> (usize, u64) {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return (0, 0);
    };
    if !meta.is_dir() {
        return (1, meta.len());
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| measure(&entry.path()))
        .fold((0, 0), |(f, b), (f2, b2)| (f + f2, b + b2))
}

/// Every set of stubs this process has served, by id, so a path handed out
/// can be written to disk when something opens it.
fn served() -> &'static Mutex<HashMap<String, Arc<Stubs>>> {
    static SERVED: OnceLock<Mutex<HashMap<String, Arc<Stubs>>>> = OnceLock::new();
    SERVED.get_or_init(|| Mutex::new(HashMap::new()))
}

impl Stubs {
    /// The stubs a store row holds, shared with any tree that already
    /// served them in this process.
    pub(crate) fn from_row(row: crate::store::Rbs) -> Arc<Stubs> {
        let id = dir_name(&row.version, &row.key);
        let mut served = served()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        served
            .entry(id.clone())
            .or_insert_with(|| {
                Arc::new(Stubs {
                    id,
                    version: row.version,
                    core_text: row.core,
                    stdlib_text: row.stdlib,
                    sigs_text: row.sigs,
                    core: OnceLock::new(),
                    stdlib: OnceLock::new(),
                    core_defs: OnceLock::new(),
                    stdlib_defs: OnceLock::new(),
                    sig_defs: OnceLock::new(),
                })
            })
            .clone()
    }

    /// `<core>/rbs-3.8.0-…/`: what each of its sites' paths starts with.
    fn prefix(&self) -> String {
        format!("{CORE_PATH}/{}/", self.id)
    }

    /// Core, one file per top-level owner.
    pub(crate) fn core_files(&self) -> &[CoreFile] {
        self.core
            .get_or_init(|| split(&self.core_text, "", &self.version))
    }

    /// The stdlib's compiled half, one file per top-level owner.
    pub(crate) fn stdlib_files(&self) -> &[CoreFile] {
        self.stdlib
            .get_or_init(|| split(&self.stdlib_text, STDLIB_DIR, &self.version))
    }

    /// A file's site path: `<core>/rbs-3.8.0-…/String.rb`.
    pub(crate) fn site_path(&self, file: &CoreFile) -> String {
        format!("{}{}", self.prefix(), file.name)
    }

    /// Core's methods, by name.
    pub(crate) fn core_defs(&self) -> &HashMap<String, Vec<StubDef>> {
        self.core_defs
            .get_or_init(|| by_name(self, self.core_files()))
    }

    /// The stdlib's compiled methods, by name.
    pub(crate) fn stdlib_defs(&self) -> &HashMap<String, Vec<StubDef>> {
        self.stdlib_defs
            .get_or_init(|| by_name(self, self.stdlib_files()))
    }

    /// The return types lent to the stdlib's Ruby methods, by (owner,
    /// singleton, name).
    pub(crate) fn sig_defs(&self) -> &HashMap<(String, bool, String), StubDef> {
        self.sig_defs.get_or_init(|| {
            let path = format!("{}{STDLIB_SIGS}", self.prefix());
            cut(&path, &self.sigs_text)
                .into_iter()
                .map(|def| ((def.owner.clone(), def.singleton, def.name.clone()), def))
                .collect()
        })
    }

    /// The whole text lent from, for a test that holds the cut to it.
    #[cfg(test)]
    pub(crate) fn sigs_text(&self) -> (String, &str) {
        (format!("{}{STDLIB_SIGS}", self.prefix()), &self.sigs_text)
    }

    /// A core file with every `def` blanked out, lines kept: its classes,
    /// mixins and constants, which a namespace is assembled from, without
    /// parsing the methods a query loads by name.
    pub(crate) fn skeleton(file: &CoreFile) -> String {
        let mut out = String::with_capacity(file.text.len());
        let mut in_def: Option<usize> = None;
        for line in file.text.lines() {
            let trimmed = line.trim_start();
            let indent = line.len() - trimmed.len();
            let blank = match in_def {
                Some(open) => {
                    if trimmed == "end" && indent == open {
                        in_def = None;
                    }
                    true
                }
                None if trimmed.starts_with("def ") => {
                    if !trimmed.ends_with("; end") {
                        in_def = Some(indent);
                    }
                    true
                }
                None => trimmed.starts_with("sig {"),
            };
            if !blank {
                out.push_str(line);
            }
            out.push('\n');
        }
        out
    }
}

fn by_name(stubs: &Stubs, files: &[CoreFile]) -> HashMap<String, Vec<StubDef>> {
    let mut by_name: HashMap<String, Vec<StubDef>> = HashMap::new();
    for file in files {
        for def in cut(&stubs.site_path(file), &file.text) {
            by_name.entry(def.name.clone()).or_default().push(def);
        }
    }
    by_name
}

/// A generated stub's `def`s, each with its owner written compactly around
/// it. Reads only the shape the generator writes — nested `class`/`module`
/// blocks, `private`/`protected` lines, `sig`s directly above each `def` —
/// and leaves the Ruby itself to the extractor.
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

/// `<core>/rbs-3.8.0-…/stdlib/Pathname.rb` → (`rbs-3.8.0-…`, `stdlib/Pathname.rb`).
fn served_path(path: &str) -> Option<(&str, &str)> {
    path.strip_prefix(CORE_PATH)?
        .strip_prefix('/')?
        .split_once('/')
}

/// Is this site a declaration in the stdlib's compiled half?
pub(crate) fn is_stdlib_stub(path: &str) -> bool {
    served_path(path).is_some_and(|(_, file)| file.starts_with(STDLIB_DIR))
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

/// A stub cut at its top-level `class`/`module` blocks, each file named for
/// its owner under `dir`.
///
/// A block takes the comment written directly above it. Code outside any
/// block — the top-level constants — goes to `Object.rb`, since a top-level
/// constant is Object's. The stub's own header describes the whole corpus and
/// is left behind; each file gets a line of its own saying what it is.
fn split(source: &str, dir: &str, version: &str) -> Vec<CoreFile> {
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
    debug_assert!(open.is_none(), "a stub ends inside a block");

    let mut files: Vec<CoreFile> = Vec::new();
    for (name, lines) in blocks {
        debug_assert!(
            !files.iter().any(|f| f.name == format!("{dir}{name}.rb")),
            "{name} is declared twice; one file cannot hold both"
        );
        let mut text = header(&name, dir, version);
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

fn header(name: &str, dir: &str, version: &str) -> String {
    let what = if dir.is_empty() {
        format!("# Ruby core: {name}, from rbs {version}'s signatures. A stub trekr")
    } else {
        format!(
            "# Ruby's stdlib: {name}, the methods compiled into it rather than\n\
             # written in Ruby, from rbs {version}'s signatures. A stub trekr"
        )
    };
    format!(
        "{what}\n# navigates by, not Ruby's source; the real documentation is\n\
         # https://docs.ruby-lang.org/en/master/{name}.html\n\n"
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

/// The directory a core site's file is written in, and its name there:
/// `<core>/rbs-3.8.0-…/String.rb` is `String.rb` in `<dir>/rbs-3.8.0-…`.
/// The RSpec stub is `RSpec.rb` in `dir` itself.
pub(crate) fn file_of(dir: &Path, path: &str) -> Option<(PathBuf, String)> {
    if is_rspec_stub(path) {
        return Some((dir.to_path_buf(), rspec_file().name.clone()));
    }
    let (id, file) = served_path(path)?;
    Some((dir.join(id), file.to_string()))
}

/// Write the RSpec stub into `dir`, and the files of every set of stubs
/// served in this process into `dir/<id>/`, rewriting only what differs so
/// an editor watching them is not churned. Once per set per directory.
pub(crate) fn materialize(dir: &Path) -> std::io::Result<()> {
    static DONE: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
    let mut done = DONE.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let sets: Vec<Arc<Stubs>> = served()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .values()
        .cloned()
        .collect();
    let mut targets: Vec<(PathBuf, Vec<&CoreFile>)> = Vec::new();
    if !done.iter().any(|d| d == dir) {
        targets.push((dir.to_path_buf(), vec![rspec_file()]));
    }
    for stubs in &sets {
        let at = dir.join(&stubs.id);
        if !done.contains(&at) {
            let files = stubs
                .core_files()
                .iter()
                .chain(stubs.stdlib_files())
                .collect();
            targets.push((at, files));
        }
    }
    for (at, files) in targets {
        std::fs::create_dir_all(at.join(STDLIB_DIR))?;
        for file in files {
            let path = at.join(&file.name);
            if std::fs::read_to_string(&path).ok().as_deref() != Some(file.text.as_str()) {
                std::fs::write(&path, &file.text)?;
            }
        }
        done.push(at);
    }
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
            "9.9.9",
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
        let files = split("class Object\nend\n\nENV = nil\n", "", "9.9.9");
        assert!(
            file(&files, "Object.rb")
                .text
                .ends_with("end\n\nENV = nil\n")
        );
    }

    #[test]
    fn the_test_core_splits_into_distinct_valid_files() {
        let stubs = super::super::test_stubs();
        let files = stubs.core_files();
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
    fn a_skeleton_keeps_every_line_and_no_def() {
        let file = CoreFile {
            name: "A.rb".into(),
            text: "class A\n  include ::B\n\n  sig { returns(::String) }\n  def x(y)\n  end\n\n  X = nil\nend\n".into(),
        };
        let skeleton = Stubs::skeleton(&file);
        assert_eq!(skeleton.lines().count(), file.text.lines().count());
        assert_eq!(
            skeleton,
            "class A\n  include ::B\n\n\n\n\n\n  X = nil\nend\n"
        );
    }

    #[test]
    fn served_paths_are_recognised_and_nothing_else_is() {
        assert!(is_core("<core>/rbs-9.9.9-abcd/String.rb"));
        assert!(is_core(RSPEC_STUB));
        assert!(!is_core("<corelib>/x.rb"));
        assert!(!is_core("lib/core.rb"));
        assert!(is_stdlib_stub("<core>/rbs-9.9.9-abcd/stdlib/Pathname.rb"));
        assert!(!is_stdlib_stub("<core>/rbs-9.9.9-abcd/String.rb"));
        assert!(!is_stdlib_stub("<core>/rbs-9.9.9-abcd/stdlib-sigs.rb"));
        assert!(!is_stdlib_stub(RSPEC_STUB));
        let dir = Path::new("/d");
        assert_eq!(
            file_of(dir, "<core>/rbs-9.9.9-abcd/stdlib/Pathname.rb"),
            Some((dir.join("rbs-9.9.9-abcd"), "stdlib/Pathname.rb".to_string()))
        );
        assert_eq!(
            file_of(dir, RSPEC_STUB),
            Some((dir.to_path_buf(), "RSpec.rb".to_string()))
        );
    }

    #[test]
    fn the_rspec_stub_is_valid_ruby_that_declares_no_class_of_its_own() {
        let facts = crate::extract::extract(rspec_file().text.as_bytes());
        assert_eq!(facts.parse_errors, 0);
        assert!(is_rspec_stub(RSPEC_STUB));
    }
}
