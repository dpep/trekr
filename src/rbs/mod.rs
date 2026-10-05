//! Ruby's own signatures, read at index time from the `rbs` gem the app's
//! Ruby carries, and written out as the stubs core and the stdlib are served
//! from (DEC-240).
//!
//! Read once per Ruby and rbs gem: the stubs are stored beside the stdlib's
//! checkout under a key that folds both, and every app on that Ruby is
//! served from the one row. A Ruby with no rbs gem has no stubs, and its
//! calls into core are answered as nothing is known of them.

mod env;
mod params;
mod parse;
mod sigs;
mod stubs;

use crate::gems::stdlib::{RbsGem, Stdlib};
use crate::store::{Roots, Store};
use sha1::{Digest, Sha1};
use std::collections::BTreeMap;
use std::path::Path;

/// What an index says of its Ruby's signatures.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct Report {
    /// The rbs gem's version.
    pub(crate) version: String,
    /// Where it was read.
    pub(crate) path: String,
    /// Why this gem: `bundled` with the Ruby, the highest `installed` for
    /// it, or `other`, another Ruby's (DEC-242).
    pub(crate) chosen: crate::gems::stdlib::Chosen,
    /// Read by this index, rather than already known.
    pub(crate) read: bool,
    /// Kept from an earlier index, since the gem found now is a worse
    /// choice (DEC-271).
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub(crate) kept: bool,
}

/// Serve the stdlib's checkout with its Ruby's signatures, reading them
/// when no index has yet. `None` when the Ruby carries no rbs gem.
pub(crate) fn prepare(store: &mut Store, stdlib: &Stdlib) -> anyhow::Result<Option<Report>> {
    let root = stdlib.root.to_string_lossy().into_owned();
    let about = store.rbs_about(&root)?;
    let gem = stdlib.rbs();
    if let Some(kept) = about.as_ref().and_then(|about| kept(about, gem.as_ref())) {
        return Ok(Some(kept));
    }
    let Some(gem) = gem else {
        store.set_rbs(&root, None, None)?;
        return Ok(None);
    };
    let key = key(&gem, &root);
    let mut report = Report {
        version: gem.version.clone(),
        path: gem.dir.to_string_lossy().into_owned(),
        chosen: gem.chosen,
        read: false,
        kept: false,
    };
    let chosen = gem.chosen.name();
    if about.is_some_and(|about| about.key == key && about.chosen == chosen) {
        return Ok(Some(report));
    }
    if store.has_rbs(&key)? {
        store.set_rbs(&root, Some((&key, chosen)), None)?;
        return Ok(Some(report));
    }
    let signatures = read(&gem.dir);
    let ruby = ruby(store, &stdlib.root, &signatures)?;
    let stubs = stubs::generate(&signatures, Some(&ruby));
    let row = crate::store::Rbs {
        key: key.clone(),
        version: gem.version,
        dir: report.path.clone(),
        core: stubs.core,
        stdlib: stubs.stdlib,
        sigs: stubs.sigs,
    };
    store.set_rbs(&root, Some((&key, chosen)), Some(&row))?;
    report.read = true;
    Ok(Some(report))
}

/// The signatures a stdlib is already served with, when the gem found now
/// is a worse choice and they are still on disk. Which rbs is found depends
/// on `$HOME` and `$GEM_HOME` — an installed one's directory, another
/// Ruby's — and a poorer environment must not take a Ruby's core away
/// (DEC-271). A gem bundled with the Ruby is always taken: nothing about
/// the environment decides that.
fn kept(about: &crate::store::RbsAbout, found: Option<&RbsGem>) -> Option<Report> {
    use crate::gems::stdlib::Chosen;
    let was = Chosen::named(&about.chosen)?;
    if !Path::new(&about.dir).join("core").is_dir() {
        return None;
    }
    let worse = match found {
        None => true,
        Some(gem) if gem.chosen == Chosen::Bundled => false,
        Some(gem) => gem
            .chosen
            .cmp(&was)
            .then_with(|| crate::gems::stdlib::version_order(&gem.version, &about.version))
            .is_lt(),
    };
    worse.then(|| Report {
        version: about.version.clone(),
        path: about.dir.clone(),
        chosen: was,
        read: false,
        kept: true,
    })
}

/// What the stubs are a function of: the stdlib they describe, the gem
/// they are read from, and the code that reads it. The gem is its path and
/// version, and when its directory, `core/` and gemspec were written: a
/// reinstall at the same version and path is read again.
fn key(gem: &RbsGem, stdlib: &str) -> String {
    let mut hash = Sha1::new();
    let written = |path: &Path| {
        std::fs::metadata(path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |at| at.as_nanos())
    };
    let spec = gem.dir.parent().and_then(Path::parent).map(|base| {
        let name = gem.dir.file_name().unwrap_or_default().to_string_lossy();
        base.join(format!("specifications/{name}.gemspec"))
    });
    let written = format!(
        "{} {} {}",
        written(&gem.dir),
        written(&gem.dir.join("core")),
        spec.as_deref().map_or(0, written)
    );
    for part in [
        stdlib,
        &gem.dir.to_string_lossy(),
        &gem.version,
        &written,
        include_str!("mod.rs"),
        include_str!("env.rs"),
        include_str!("params.rs"),
        include_str!("parse.rs"),
        include_str!("sigs.rs"),
        include_str!("stubs.rs"),
    ] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// An rbs gem's signatures: `core/`, less rubygems' (whose Ruby the stdlib
/// holds, and indexes as much of as apps call), and each library of
/// `stdlib/` with the libraries its manifest says it needs.
pub(crate) fn read(dir: &Path) -> stubs::Signatures {
    let core = rbs_files(&dir.join("core"))
        .into_iter()
        .filter(|path| !path.starts_with(dir.join("core/rubygems")))
        .filter_map(|path| source(&path, None))
        .collect();
    let mut libraries = BTreeMap::new();
    for entry in std::fs::read_dir(dir.join("stdlib"))
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let library = entry.path();
        let files: Vec<env::Source> = rbs_files(&library)
            .into_iter()
            .filter_map(|path| source(&path, Some(&name)))
            .collect();
        let deps = std::fs::read_dir(&library)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|version| crate::scan::read_text(version.path().join("manifest.yaml")).ok())
            .flat_map(|manifest| dependencies(&manifest))
            .collect();
        libraries.insert(name, (files, deps));
    }
    stubs::Signatures { core, libraries }
}

fn source(path: &Path, library: Option<&str>) -> Option<env::Source> {
    let text = crate::scan::read_text(path).ok()?;
    Some(env::Source {
        library: library.map(str::to_string),
        parsed: parse::parse(&text),
    })
}

/// Every `.rbs` under `dir`, in a stable order.
fn rbs_files(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rbs") {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// `dependencies:\n  - name: stringio` → `stringio`.
fn dependencies(manifest: &str) -> Vec<String> {
    manifest
        .lines()
        .filter_map(|line| {
            let name = line
                .trim()
                .strip_prefix('-')?
                .trim()
                .strip_prefix("name:")?;
            Some(name.trim().trim_matches(['"', '\'']).to_string())
        })
        .collect()
}

/// What the stdlib's Ruby says of itself, from the index's facts: which
/// methods it defines, which classes it opens and subclasses, and which
/// files a compiled extension backs. The files the index leaves out of a
/// library RBS describes (`json/add/`) are read here too, for what they
/// define: Ruby, so never compiled.
fn ruby(store: &Store, root: &Path, signatures: &stubs::Signatures) -> anyhow::Result<stubs::Ruby> {
    let root_str = root.to_string_lossy().into_owned();
    let roots = Roots {
        list: vec![root_str],
        ..Roots::default()
    };
    let mut ruby = stubs::Ruby {
        files: crate::gems::stdlib::paths(root).into_iter().collect(),
        compiled_features: crate::gems::stdlib::compiled_features(root),
        ..stubs::Ruby::default()
    };
    // Owners and superclasses as a tree resolves them: `class << Time`
    // inside `class Time`, `def HTTP.new` inside `module Net`.
    let tree = crate::tree::Tree::alone(store, &roots.list[0])?;
    for row in store.methods(&roots)? {
        let owner = match tree.owner(&row) {
            owner if owner.is_empty() => "Object".to_string(),
            owner => owner,
        };
        let params = row
            .params
            .iter()
            .enumerate()
            .filter_map(|(i, p)| runtime_param(i, p))
            .collect();
        ruby.methods
            .entry((owner, row.singleton, row.name))
            .or_insert((params, row.visibility));
    }
    for (fqn, kind) in tree.declared() {
        let kind = match kind.as_str() {
            "class" => parse::Kind::Class,
            "module" => parse::Kind::Module,
            _ => continue,
        };
        if kind == parse::Kind::Class {
            let ancestry = tree.ancestors(&fqn);
            let superclass = ancestry
                .chain
                .iter()
                .skip_while(|name| **name != fqn)
                .skip(1)
                .find(|name| tree.kind_of(name) == Some("class"));
            ruby.superclasses.extend(superclass.cloned());
            // Outside the stdlib (`< StandardError`): named as written. A
            // module among them is never a return anyway.
            ruby.superclasses.extend(
                ancestry
                    .unresolved
                    .iter()
                    .map(|name| name.trim_start_matches("::").to_string()),
            );
        }
        ruby.declared.entry(fqn).or_insert(kind);
    }
    let prefix = format!("{}/", root.to_string_lossy());
    for marker in store.dynamic_markers(&roots)? {
        if marker.target.starts_with(crate::core::COMPILED) {
            if let Some(path) = marker.path.strip_prefix(&prefix) {
                ruby.compiled_files.insert(path.to_string());
            }
            continue;
        }
        let maker = crate::core::Maker::parse(&marker.target);
        // Only a maker that spells the names' shape rules any out; a macro
        // makes them on its callers, not here.
        if maker.shape.is_some() && maker.via.is_none() {
            ruby.made
                .entry(written(&marker.owner))
                .or_default()
                .push(maker);
        }
    }
    let libraries = stubs::libraries_of(signatures, &ruby);
    let unindexed = crate::scan::walk(root, "")
        .into_keys()
        .filter(|path| crate::gems::stdlib::skipped(path))
        .filter(|path| {
            libraries
                .iter()
                .any(|library| stubs::belongs(path, library))
        });
    for path in unindexed {
        let Ok(source) = crate::scan::read_source(root.join(&path)) else {
            continue;
        };
        for def in crate::extract::extract(&source).defs {
            match def.kind {
                crate::core::Kind::Method if def.via.is_none() => {
                    let owner = owner(&def.nesting, def.target.as_deref(), def.singleton, None);
                    ruby.unindexed.insert((owner, def.singleton, def.name));
                }
                crate::core::Kind::Class | crate::core::Kind::Module => {
                    let kind = if def.kind == crate::core::Kind::Class {
                        parse::Kind::Class
                    } else {
                        parse::Kind::Module
                    };
                    let mut nesting = def.nesting.clone();
                    nesting.insert(0, def.name.clone());
                    ruby.declared.entry(written(&nesting)).or_insert(kind);
                }
                _ => {}
            }
        }
    }
    Ok(ruby)
}

/// The class a method row is defined on, as far as its own text says: its
/// scopes, or the receiver of `def Foo.x`. A top-level `def` is Object's.
fn owner(nesting: &[String], target: Option<&str>, singleton: bool, via: Option<&str>) -> String {
    let owner = match target {
        Some(target) if singleton && via.is_none() && target != "self" => {
            target.trim_start_matches("::").to_string()
        }
        _ => written(nesting),
    };
    if owner.is_empty() {
        "Object".to_string()
    } else {
        owner
    }
}

/// A scope stack, innermost first, as the name it spells: `["B", "A"]` is
/// `A::B`, and `["::C", "A"]` is `C`.
fn written(nesting: &[String]) -> String {
    let mut name = String::new();
    for part in nesting.iter().rev() {
        match part.strip_prefix("::") {
            Some(absolute) => name = absolute.to_string(),
            None if name.is_empty() => name = part.clone(),
            None => name = format!("{name}::{part}"),
        }
    }
    name
}

/// A Ruby method's parameter as `Method#parameters` would name it: an
/// anonymous one by its position or its role.
fn runtime_param(i: usize, param: &crate::core::Param) -> Option<params::Param> {
    use crate::core::ParamKind as K;
    use params::ParamKind as P;
    let (kind, fallback) = match param.kind {
        K::Req | K::Post => (P::Req, format!("arg{}", i + 1)),
        K::Opt => (P::Opt, format!("arg{}", i + 1)),
        K::Rest => (P::Rest, "args".to_string()),
        K::Keyreq => (P::Keyreq, format!("key{}", i + 1)),
        K::Key => (P::Key, format!("key{}", i + 1)),
        K::Keyrest => (P::Keyrest, "options".to_string()),
        K::Block => (P::Block, "block".to_string()),
        K::Nokey => return None,
    };
    let named = params::is_ident(&param.name) && !params::KEYWORDS.contains(&param.name.as_str());
    let name = if named { param.name.clone() } else { fallback };
    Some(params::Param::new(kind, name))
}

/// The stubs for this rbs directory with no stdlib indexed: core alone.
#[cfg(test)]
pub(crate) fn core_only(dir: &Path) -> stubs::Stubs {
    stubs::generate(&read(dir), None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_manifests_dependencies() {
        assert_eq!(
            dependencies("dependencies:\n  - name: dbm\n  - name: 'pstore'\n"),
            ["dbm", "pstore"]
        );
    }

    #[test]
    fn an_anonymous_parameter_is_named_by_its_role() {
        let param = |kind, name: &str| crate::core::Param {
            kind,
            name: name.to_string(),
        };
        use crate::core::ParamKind as K;
        assert_eq!(runtime_param(0, &param(K::Rest, "*")).unwrap().name, "args");
        assert_eq!(runtime_param(1, &param(K::Req, "")).unwrap().name, "arg2");
        assert_eq!(
            runtime_param(0, &param(K::Req, "path")).unwrap().name,
            "path"
        );
        assert!(runtime_param(0, &param(K::Nokey, "")).is_none());
    }

    #[test]
    fn nesting_spells_the_name() {
        assert_eq!(written(&["B".into(), "A".into()]), "A::B");
        assert_eq!(written(&["C::D".into()]), "C::D");
        assert_eq!(
            written(&["::Digest::Class".into(), "Digest".into()]),
            "Digest::Class"
        );
        assert_eq!(owner(&[], None, false, None), "Object");
    }
}
