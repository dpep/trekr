//! Ruby's core and its stdlib's compiled half, written as the Ruby stubs the
//! tree is served from, from the rbs gem the app's Ruby carries (DEC-240).
//!
//! Three texts, each read by the ordinary extractor:
//!
//! - **core**: every class, module and method RBS writes for Ruby core,
//!   with its parameters and the `sig`s DEC-077's rules allow;
//! - **stdlib**: the stdlib's methods no Ruby file defines — compiled into
//!   an extension (`Pathname#read`), as RBS declares them — and the classes
//!   no Ruby file declares (`Digest::SHA256`);
//! - **sigs**: the return types of stdlib methods its Ruby does define,
//!   lent to the real `def` (DEC-220). Never a location.
//!
//! Which methods are compiled is inferred rather than asked of a Ruby: a
//! method RBS writes that the indexed stdlib's Ruby does not define, on a
//! class a compiled extension backs or no Ruby file declares.

use super::env::{Class, Env, Library, Method, Source, is_unnamed};
use super::params::{self, Param, ParamKind};
use super::parse::{Kind, Mixin, Vis};
use super::sigs;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

/// What the stdlib's Ruby says, read from its facts.
#[derive(Debug, Default)]
pub(crate) struct Ruby {
    /// Every method its indexed Ruby defines, `(owner, singleton, name)`,
    /// with its parameters and visibility.
    pub(crate) methods: HashMap<(String, bool, String), (Vec<Param>, String)>,
    /// Every method a file the index leaves out writes with `def`
    /// (`json/add/`'s `Time#to_json`): Ruby, so not compiled, and opt-in, so
    /// not lent. Not an alias: `alias start sg unless method_defined?(:start)`
    /// stands in for a compiled method that is usually there.
    pub(crate) unindexed: HashSet<(String, bool, String)>,
    /// Every class or module a Ruby file opens, indexed or not.
    pub(crate) declared: HashMap<String, Kind>,
    /// The files a compiled extension backs (DEC-181): a library with one
    /// has C methods its Ruby does not write.
    pub(crate) compiled_files: HashSet<String>,
    /// The names Ruby makes at runtime on a class, where its source spells
    /// their shape (`Ripper::SexpBuilder`'s `on_*`): not compiled.
    pub(crate) made: HashMap<String, Vec<crate::core::Maker>>,
    /// Every class its Ruby names as a superclass.
    pub(crate) superclasses: HashSet<String>,
    /// Its indexed files, relative to its root: which libraries it has.
    pub(crate) files: HashSet<String>,
}

/// Is this stdlib path one of this rbs library's files (`net-http` →
/// `net/http.rb` and `net/http/`)?
pub(crate) fn belongs(path: &str, library: &str) -> bool {
    [library.to_string(), library.replace('-', "/")]
        .iter()
        .any(|feature| {
            path.strip_prefix(feature.as_str())
                .is_some_and(|rest| rest == ".rb" || rest.starts_with('/'))
        })
}

/// The rbs gem's signatures, read.
pub(crate) struct Signatures {
    pub(crate) core: Vec<Source>,
    /// Library → its files, and the libraries it depends on.
    pub(crate) libraries: BTreeMap<String, (Vec<Source>, Vec<String>)>,
}

#[derive(Debug, Default, PartialEq)]
pub(crate) struct Stubs {
    pub(crate) core: String,
    pub(crate) stdlib: String,
    pub(crate) sigs: String,
    /// Members of the rbs files that could not be read, and were skipped.
    pub(crate) skipped: usize,
}

/// One `def` of a stub.
struct Entry {
    singleton: bool,
    name: String,
    params: Vec<Param>,
    sigs: Vec<String>,
    visibility: String,
}

/// A class or module as a stub writes it.
#[derive(Default)]
struct Owner {
    kind: Option<Kind>,
    superclass: Option<String>,
    mixins: Vec<(Mixin, String)>,
    /// `NAME = nil`, or `NAME = ::Target` for a class alias.
    constants: Vec<(String, Option<String>)>,
    entries: Vec<Entry>,
}

pub(crate) fn generate(signatures: &Signatures, ruby: Option<&Ruby>) -> Stubs {
    let core_env = Env::build(&signatures.core);
    let core_known = returnable(&core_env);
    let core = render_core(&core_env, &core_known);
    let mut stubs = Stubs {
        core,
        skipped: core_env.skipped,
        ..Stubs::default()
    };
    if let Some(ruby) = ruby {
        stdlib(signatures, ruby, &core_env, &core_known, &mut stubs);
    }
    stubs
}

/// The classes a return may name: declared, never a module, and subclassed
/// by nothing — `Numeric#+` "returns a Numeric", and an Integer then finds
/// `to_s` in the wrong class. Enumerator is the exception: only `lazy`
/// makes its subclass. A class object answers with its own singleton
/// methods, so neither `Class` nor `Module` (DEC-077).
fn returnable(env: &Env) -> HashSet<String> {
    let subclassed: HashSet<&str> = env
        .classes
        .values()
        .filter_map(|c| c.superclass.as_deref())
        .collect();
    env.classes
        .values()
        .filter(|c| c.kind == Kind::Class && shown(&c.name))
        .map(|c| c.name.as_str())
        .filter(|name| !subclassed.contains(name) || *name == "Enumerator")
        .filter(|name| !["Class", "Module"].contains(name))
        .map(str::to_string)
        .collect()
}

/// A class a stub may write: not one of RBS's own names for what Ruby
/// leaves unnamed.
fn shown(name: &str) -> bool {
    name != "RBS" && !name.starts_with("RBS::")
}

fn visibility(vis: Vis) -> String {
    match vis {
        Vis::Public => "public",
        Vis::Private => "private",
    }
    .to_string()
}

fn entry(method: &Method, known: &HashSet<String>) -> Entry {
    let params = params::for_method(method);
    let sigs = sigs::sigs(method, known, &params);
    Entry {
        singleton: method.singleton,
        name: method.name.clone(),
        params,
        sigs,
        visibility: visibility(method.visibility),
    }
}

/// A mixin a stub writes: a module the stub's world declares, not an
/// interface and not one of RBS's unnamed ones (whose methods are already
/// the includer's own).
fn writable_mixins(class: &Class, env: &Env) -> Vec<(Mixin, String)> {
    class
        .mixins
        .iter()
        .filter(|(_, target)| {
            !is_unnamed(target)
                && shown(target)
                && env
                    .classes
                    .get(target)
                    .is_some_and(|c| c.kind == Kind::Module)
        })
        .cloned()
        .collect()
}

fn render_core(env: &Env, known: &HashSet<String>) -> String {
    let mut owners: BTreeMap<String, Owner> = BTreeMap::new();
    for class in env.classes.values() {
        if class.kind == Kind::Interface || !shown(&class.name) {
            continue;
        }
        let owner = owners.entry(class.name.clone()).or_default();
        owner.kind = Some(class.kind);
        owner.superclass = class
            .superclass
            .clone()
            .filter(|s| class.kind == Kind::Class && s != "Object");
        owner.mixins = writable_mixins(class, env);
        owner.constants = class.constants.iter().map(|c| (c.clone(), None)).collect();
        owner.entries = class.methods.values().map(|m| entry(m, known)).collect();
    }
    // A constant written from the top (`Process::CLOCK_REALTIME: Integer`)
    // belongs in its namespace's file, like one written in its body.
    let mut loose: Vec<String> = Vec::new();
    for name in &env.constants {
        match name.rsplit_once("::") {
            Some((outer, last)) if owners.contains_key(outer) => owners
                .get_mut(outer)
                .expect("just checked")
                .constants
                .push((last.to_string(), None)),
            _ => loose.push(format!("{name} = nil")),
        }
    }
    for (alias, target) in &env.class_aliases {
        if !shown(target) {
            continue;
        }
        match alias.rsplit_once("::") {
            Some((outer, name)) if owners.contains_key(outer) => owners
                .get_mut(outer)
                .expect("just checked")
                .constants
                .push((name.to_string(), Some(target.clone()))),
            Some(_) => {}
            None => loose.push(format!("{alias} = ::{target}")),
        }
    }
    let mut out = String::from(
        "# Ruby core, from the rbs gem's signatures for it: a stub trekr navigates\n\
         # by, written when the Ruby's stdlib is indexed (DEC-240).\n",
    );
    render(&owners, &mut out);
    if !loose.is_empty() {
        out.push('\n');
        for line in loose {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out
}

/// Every owner, each nested in the namespaces around it; an owner whose
/// namespace is not among them is left out, since it could not be opened.
fn render(owners: &BTreeMap<String, Owner>, out: &mut String) {
    let tops: BTreeSet<&str> = owners
        .keys()
        .filter(|name| !name.contains("::"))
        .map(String::as_str)
        .collect();
    for top in tops {
        out.push('\n');
        render_owner(top, owners, 0, out);
    }
}

fn render_owner(fqn: &str, owners: &BTreeMap<String, Owner>, depth: usize, out: &mut String) {
    let owner = &owners[fqn];
    let indent = "  ".repeat(depth);
    let inner = "  ".repeat(depth + 1);
    let short = fqn.rsplit("::").next().unwrap_or(fqn);
    let kind = match owner.kind {
        Some(Kind::Module) => "module",
        _ => "class",
    };
    out.push_str(&format!("{indent}{kind} {short}"));
    if let Some(superclass) = &owner.superclass {
        out.push_str(&format!(" < ::{superclass}"));
    }
    out.push('\n');
    let mut body: Vec<String> = Vec::new();
    for (mixin, target) in &owner.mixins {
        let word = match mixin {
            Mixin::Include => "include",
            Mixin::Extend => "extend",
            Mixin::Prepend => "prepend",
        };
        body.push(format!("{inner}{word} ::{target}"));
    }
    if !owner.constants.is_empty() {
        if !body.is_empty() {
            body.push(String::new());
        }
        for (name, target) in &owner.constants {
            match target {
                Some(target) => body.push(format!("{inner}{name} = ::{target}")),
                None => body.push(format!("{inner}{name} = nil")),
            }
        }
    }
    // Class methods first: a `private` above them would read as theirs.
    for singleton in [true, false] {
        let mut group: Vec<&Entry> = owner
            .entries
            .iter()
            .filter(|e| e.singleton == singleton)
            .collect();
        group.sort_by(|a, b| a.name.cmp(&b.name));
        for vis in ["public", "protected", "private"] {
            let listed: Vec<&&Entry> = group.iter().filter(|e| e.visibility == vis).collect();
            if listed.is_empty() {
                continue;
            }
            if !body.is_empty() {
                body.push(String::new());
            }
            if vis != "public" {
                body.push(format!("{inner}{vis}"));
            }
            for (i, entry) in listed.iter().enumerate() {
                if i > 0 {
                    body.push(String::new());
                }
                for sig in &entry.sigs {
                    body.push(format!("{inner}{sig}"));
                }
                let signature = if entry.params.is_empty() {
                    String::new()
                } else {
                    format!("({})", params::render(&entry.params))
                };
                let side = if entry.singleton { "self." } else { "" };
                body.push(format!("{inner}def {side}{}{signature}", entry.name));
                body.push(format!("{inner}end"));
            }
        }
    }
    let prefix = format!("{fqn}::");
    let children: Vec<&String> = owners
        .keys()
        .filter(|name| {
            name.strip_prefix(&prefix)
                .is_some_and(|rest| !rest.contains("::"))
        })
        .collect();
    for child in children {
        if !body.is_empty() {
            body.push(String::new());
        }
        let mut nested = String::new();
        render_owner(child, owners, depth + 1, &mut nested);
        body.extend(nested.lines().map(str::to_string));
    }
    for line in body {
        out.push_str(&line);
        out.push('\n');
    }
    out.push_str(&format!("{indent}end\n"));
}

/// The libraries a stdlib has: an rbs library whose file `require` loads
/// is among the indexed stdlib's (`net-http` → `net/http.rb`).
pub(crate) fn libraries_of(signatures: &Signatures, files: &HashSet<String>) -> BTreeSet<String> {
    signatures
        .libraries
        .keys()
        .filter(|library| {
            [library.to_string(), library.replace('-', "/")]
                .iter()
                .any(|feature| files.contains(&format!("{feature}.rb")))
        })
        .cloned()
        .collect()
}

fn stdlib(
    signatures: &Signatures,
    ruby: &Ruby,
    core_env: &Env,
    core_known: &HashSet<String>,
    stubs: &mut Stubs,
) {
    let libraries = libraries_of(signatures, &ruby.files);
    // A library's dependencies are loaded to resolve its names, and are not
    // themselves described: nothing indexed would own their stubs.
    let mut loaded: BTreeSet<String> = BTreeSet::new();
    let mut stack: Vec<String> = libraries.iter().cloned().collect();
    while let Some(library) = stack.pop() {
        if !loaded.insert(library.clone()) {
            continue;
        }
        if let Some((_, deps)) = signatures.libraries.get(&library) {
            stack.extend(deps.iter().cloned());
        }
    }
    let mut sources: Vec<&Source> = signatures.core.iter().collect();
    for library in &loaded {
        if let Some((files, _)) = signatures.libraries.get(library) {
            sources.extend(files);
        }
    }
    let mut env = Env::build(sources.iter().copied());
    env.derive_new();
    stubs.skipped += env.skipped;

    let described = |library: &Library| library.as_ref().is_some_and(|l| libraries.contains(l));
    // A library compiles part of itself when an extension backs one of its
    // files. Only such a library's RBS can describe what no Ruby writes.
    let extended: HashSet<&String> = libraries
        .iter()
        .filter(|library| {
            ruby.compiled_files
                .iter()
                .any(|path| belongs(path, library))
        })
        .collect();
    let compiles = |library: &Library| library.as_ref().is_some_and(|l| extended.contains(l));
    // Every class a compiled library's RBS declares and no Ruby file does,
    // whether or not it writes a method of its own: `Digest::SHA256`
    // inherits all of them.
    let library_classes: Vec<&Class> = env
        .classes
        .values()
        .filter(|c| c.kind != Kind::Interface && shown(&c.name))
        .filter(|c| c.libraries.iter().all(described) && c.libraries.iter().all(compiles))
        .filter(|c| !ruby.declared.contains_key(&c.name))
        .collect();

    let subclassed: HashSet<&str> = env
        .classes
        .values()
        .filter_map(|c| c.superclass.as_deref())
        .chain(ruby.superclasses.iter().map(String::as_str))
        .collect();
    let kind_of = |name: &str| -> Option<Kind> {
        env.classes
            .get(name)
            .map(|c| c.kind)
            .filter(|k| *k != Kind::Interface)
            .or_else(|| ruby.declared.get(name).copied())
    };

    struct Candidate<'a> {
        owner: &'a str,
        method: &'a Method,
        compiled: bool,
    }
    let mut candidates: Vec<Candidate> = Vec::new();
    for class in env.classes.values() {
        if class.kind == Kind::Interface || !shown(&class.name) {
            continue;
        }
        for method in class.methods.values() {
            // Core's own methods are the core stub's; `initialize` is every
            // class's own, and the core stub's variadic one is honest.
            if !described(&method.library) || method.name == "initialize" {
                continue;
            }
            let key = (class.name.clone(), method.singleton, method.name.clone());
            let in_core = core_env.classes.get(&class.name).is_some_and(|c| {
                c.methods
                    .contains_key(&(method.singleton, method.name.clone()))
            });
            let made = ruby.made.get(&class.name).is_some_and(|makers| {
                makers
                    .iter()
                    .any(|maker| maker.may_make(&method.name, method.singleton))
            });
            // Written in Ruby lends its return; otherwise compiled, unless
            // the core stub has it, a file the index leaves out writes it,
            // Ruby makes it at runtime, or its library compiles nothing.
            let compiled = if ruby.methods.contains_key(&key) {
                false
            } else if in_core || ruby.unindexed.contains(&key) || made || !compiles(&method.library)
            {
                continue;
            } else {
                true
            };
            candidates.push(Candidate {
                owner: &class.name,
                method,
                compiled,
            });
        }
    }

    // The classes whose kind a stub must say: every owner, and every
    // namespace around one.
    let mut named: BTreeSet<String> = BTreeSet::new();
    for owner in candidates
        .iter()
        .map(|c| c.owner)
        .chain(library_classes.iter().map(|c| c.name.as_str()))
    {
        let parts: Vec<&str> = owner.split("::").collect();
        for i in 1..=parts.len() {
            named.insert(parts[..i].join("::"));
        }
    }
    let core_declared: HashSet<&str> = core_env.classes.keys().map(String::as_str).collect();
    let core_modules: HashSet<&str> = core_env
        .classes
        .values()
        .filter(|c| c.kind == Kind::Module)
        .map(|c| c.name.as_str())
        .collect();
    let mut known: HashSet<String> = core_known
        .iter()
        .filter(|name| !core_modules.contains(name.as_str()))
        .cloned()
        .collect();
    known.extend(
        named
            .iter()
            .filter(|name| kind_of(name) == Some(Kind::Class))
            .filter(|name| !subclassed.contains(name.as_str()))
            .filter(|name| !core_declared.contains(name.as_str()))
            .cloned(),
    );

    let mut compiled_owners: BTreeMap<String, Owner> = BTreeMap::new();
    let mut written_owners: BTreeMap<String, Owner> = BTreeMap::new();
    for candidate in &candidates {
        let key = (
            candidate.owner.to_string(),
            candidate.method.singleton,
            candidate.method.name.clone(),
        );
        let (params, vis) = if candidate.compiled {
            let params = params::spellable(params::for_method(candidate.method));
            (params, visibility(candidate.method.visibility))
        } else {
            let (mut params, vis) = ruby.methods[&key].clone();
            // A block's `sig` names the block parameter, which a method
            // that `yield`s does not write.
            if candidate.method.overloads.iter().any(|o| o.block.is_some())
                && !params.iter().any(|p| p.kind == ParamKind::Block)
            {
                params.push(Param::new(ParamKind::Block, "block"));
            }
            (params, vis)
        };
        let sigs = sigs::sigs(candidate.method, &known, &params);

        if sigs.is_empty() && !candidate.compiled {
            continue;
        }
        let target = if candidate.compiled {
            &mut compiled_owners
        } else {
            &mut written_owners
        };
        target
            .entry(candidate.owner.to_string())
            .or_default()
            .entries
            .push(Entry {
                singleton: candidate.method.singleton,
                name: candidate.method.name.clone(),
                params,
                sigs,
                visibility: vis,
            });
    }
    for class in &library_classes {
        compiled_owners.entry(class.name.clone()).or_default();
    }
    for owners in [&mut compiled_owners, &mut written_owners] {
        // Every namespace around an owner, so each can be nested; an owner
        // with one nothing declares is dropped.
        let names: Vec<String> = owners.keys().cloned().collect();
        for name in names {
            let parts: Vec<&str> = name.split("::").collect();
            let prefixes: Vec<String> = (1..=parts.len()).map(|i| parts[..i].join("::")).collect();
            if prefixes.iter().any(|p| kind_of(p).is_none()) {
                owners.remove(&name);
                continue;
            }
            for prefix in prefixes {
                owners.entry(prefix).or_default();
            }
        }
        for (name, owner) in owners.iter_mut() {
            owner.kind = kind_of(name);
            if let Some(class) = env.classes.get(name) {
                owner.superclass = class
                    .superclass
                    .clone()
                    .filter(|s| class.kind == Kind::Class && s != "Object");
                owner.mixins = writable_mixins(class, &env)
                    .into_iter()
                    .filter(|(kind, _)| *kind == Mixin::Include)
                    .collect();
            }
        }
    }
    stubs.stdlib = String::from(
        "# Ruby's standard library, the half its Ruby does not write: methods compiled\n\
         # into an extension, as RBS describes them (DEC-240). Every method is a\n\
         # declaration; a class counts only where no Ruby file declares it.\n",
    );
    render(&compiled_owners, &mut stubs.stdlib);
    stubs.sigs = String::from(
        "# Return types for stdlib methods written in Ruby, which lend them to the\n\
         # real definitions (DEC-240). Never a location.\n",
    );
    render(&written_owners, &mut stubs.sigs);
}
