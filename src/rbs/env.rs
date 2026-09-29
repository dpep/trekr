//! RBS files merged into one set of classes, each name resolved the way RBS
//! resolves it: through the lexical scopes around where it is written.

use super::parse::{self, Decl, Kind, Member, Mixin, Side, Ty, Vis};
use std::collections::{BTreeMap, HashSet};

/// Where a method or class came from: core, or a stdlib library by its
/// directory name (`pathname`, `net-http`).
pub(crate) type Library = Option<String>;

/// What an overload returns, once its names are resolved.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Returns {
    /// An instance of this class or module, by its full name.
    Class(String),
    /// Anything else: a union, an optional, `self`, `bool`, a type variable,
    /// an interface, an alias.
    Other,
}

#[derive(Clone, Debug)]
pub(crate) struct Overload {
    pub(crate) function: Option<parse::Function>,
    pub(crate) block: Option<bool>,
    pub(crate) returns: Returns,
}

#[derive(Clone, Debug)]
pub(crate) struct Method {
    pub(crate) name: String,
    pub(crate) singleton: bool,
    pub(crate) visibility: Vis,
    pub(crate) overloads: Vec<Overload>,
    /// Every comment written over it, the call-seq's source.
    pub(crate) comments: Vec<String>,
    pub(crate) library: Library,
}

#[derive(Clone, Debug)]
pub(crate) struct Class {
    pub(crate) name: String,
    pub(crate) kind: Kind,
    pub(crate) superclass: Option<String>,
    pub(crate) mixins: Vec<(Mixin, String)>,
    /// A module's self types (`Kernel : BasicObject`).
    pub(crate) self_types: Vec<String>,
    /// `(singleton, name)` → method, as written in this class's own
    /// declarations — not an ancestor's.
    pub(crate) methods: BTreeMap<(bool, String), Method>,
    pub(crate) constants: Vec<String>,
    /// The library of each declaration of it.
    pub(crate) libraries: Vec<Library>,
}

/// An `alias`, waiting for every method it could name to be read.
struct PendingAlias {
    owner: String,
    new: String,
    old: String,
    singleton: bool,
    comment: String,
    library: Library,
    visibility: Vis,
}

#[derive(Default)]
pub(crate) struct Env {
    pub(crate) classes: BTreeMap<String, Class>,
    /// `class Mutex = Thread::Mutex`: alias → target, both full names.
    pub(crate) class_aliases: BTreeMap<String, String>,
    /// Top-level constants.
    pub(crate) constants: Vec<String>,
    /// Members no parse could read, across every file.
    pub(crate) skipped: usize,
}

/// One file's declarations, with the library it belongs to.
pub(crate) struct Source {
    pub(crate) library: Library,
    pub(crate) parsed: parse::Parsed,
}

/// A declaration, with the full names of the scopes it is written in.
struct Scoped<'a> {
    decl: &'a Decl,
    name: String,
    context: Vec<String>,
    library: &'a Library,
}

impl Env {
    pub(crate) fn build<'a>(sources: impl IntoIterator<Item = &'a Source>) -> Env {
        let mut env = Env::default();
        let mut scoped: Vec<Scoped> = Vec::new();
        for source in sources {
            env.skipped += source.parsed.skipped;
            for member in &source.parsed.members {
                match member {
                    Member::Decl(decl) => collect(decl, &[], &source.library, &mut scoped),
                    Member::Constant(name) => env
                        .constants
                        .push(name.trim_start_matches("::").to_string()),
                    _ => {}
                }
            }
        }
        // Every name first, so a reference resolves whatever order the files
        // came in.
        let declared: HashSet<String> = scoped.iter().map(|s| s.name.clone()).collect();
        let resolve = |written: &str, context: &[String]| resolve(written, context, &declared);

        let mut pending: Vec<PendingAlias> = Vec::new();
        for s in &scoped {
            if let Some(target) = &s.decl.alias_of {
                env.class_aliases
                    .insert(s.name.clone(), resolve(target, &s.context));
                continue;
            }
            // Names inside a class resolve from the class outward.
            let mut inner = s.context.clone();
            inner.push(s.name.clone());
            let class = env.classes.entry(s.name.clone()).or_insert_with(|| Class {
                name: s.name.clone(),
                kind: s.decl.kind,
                superclass: None,
                mixins: Vec::new(),
                self_types: Vec::new(),
                methods: BTreeMap::new(),
                constants: Vec::new(),
                libraries: Vec::new(),
            });
            class.libraries.push(s.library.clone());
            if class.superclass.is_none()
                && let Some(superclass) = &s.decl.superclass
            {
                // Ruby evaluates a superclass outside the body it opens.
                class.superclass = Some(resolve(superclass, &s.context));
            }
            for name in &s.decl.self_types {
                let target = resolve(name, &inner);
                if !class.self_types.contains(&target) {
                    class.self_types.push(target);
                }
            }
            for member in &s.decl.members {
                match member {
                    Member::Mixin(kind, name) => {
                        let target = resolve(name, &inner);
                        if !class.mixins.contains(&(*kind, target.clone())) {
                            class.mixins.push((*kind, target));
                        }
                    }
                    Member::Constant(name) => class.constants.push(name.clone()),
                    Member::Method(m) => {
                        let sides: &[bool] = match m.side {
                            Side::Instance => &[false],
                            Side::Singleton => &[true],
                            Side::Both => &[false, true],
                        };
                        for &singleton in sides {
                            let visibility = match (m.side, singleton) {
                                // A module function's instance half is private.
                                (Side::Both, false) => Vis::Private,
                                (Side::Both, true) => Vis::Public,
                                _ => m.visibility,
                            };
                            let class_params = &s.decl.type_params;
                            let overloads: Vec<Overload> = m
                                .overloads
                                .iter()
                                .map(|o| Overload {
                                    function: o.function.clone(),
                                    block: o.block,
                                    returns: returns(
                                        &o.returns,
                                        class_params,
                                        &o.type_params,
                                        &inner,
                                        &declared,
                                    ),
                                })
                                .collect();
                            add_method(
                                class,
                                &m.name,
                                singleton,
                                visibility,
                                overloads,
                                m.overloading,
                                &m.comment,
                                s.library,
                            );
                        }
                    }
                    Member::Alias {
                        new,
                        old,
                        singleton,
                        comment,
                    } => pending.push(PendingAlias {
                        owner: s.name.clone(),
                        new: new.clone(),
                        old: old.clone(),
                        singleton: *singleton,
                        comment: comment.clone(),
                        library: s.library.clone(),
                        visibility: Vis::Public,
                    }),
                    Member::Decl(_) => {}
                }
            }
        }
        // A class RBS derives from one of its unnamed classes (`Random <
        // RBS::Unnamed::Random_Base`) has that class's members as its own,
        // and its superclass.
        let derived: Vec<String> = env
            .classes
            .values()
            .filter(|c| c.superclass.as_deref().is_some_and(is_unnamed))
            .map(|c| c.name.clone())
            .collect();
        for name in derived {
            let mut seen = HashSet::new();
            while let Some(base) = env.classes[&name]
                .superclass
                .clone()
                .filter(|s| is_unnamed(s))
            {
                let Some(source) = env
                    .classes
                    .get(&base)
                    .cloned()
                    .filter(|_| seen.insert(base.clone()))
                else {
                    env.classes.get_mut(&name).expect("listed").superclass = None;
                    break;
                };
                let class = env.classes.get_mut(&name).expect("listed");
                class.superclass = source.superclass;
                for mixin in source.mixins {
                    if !class.mixins.contains(&mixin) {
                        class.mixins.push(mixin);
                    }
                }
                for (key, method) in source.methods {
                    class.methods.entry(key).or_insert(method);
                }
            }
        }
        // Methods written in RBS's unnamed modules (`Random::Formatter`'s)
        // are the including module's own.
        let unnamed: Vec<(String, Vec<(Mixin, String)>)> = env
            .classes
            .values()
            .map(|c| (c.name.clone(), c.mixins.clone()))
            .collect();
        for (owner, mixins) in unnamed {
            for (kind, target) in mixins {
                if !is_unnamed(&target) || kind == Mixin::Prepend {
                    continue;
                }
                let Some(source) = env.classes.get(&target).cloned() else {
                    continue;
                };
                let singleton = kind == Mixin::Extend;
                let class = env.classes.get_mut(&owner).expect("an owner just listed");
                for ((side, name), method) in source.methods {
                    if side {
                        continue;
                    }
                    class.methods.entry((singleton, name)).or_insert(Method {
                        singleton,
                        ..method
                    });
                }
            }
        }
        for alias in pending {
            let found = env.find(
                &alias.owner,
                alias.singleton,
                &alias.old,
                &mut HashSet::new(),
            );
            let Some(found) = found else {
                continue;
            };
            let class = env.classes.get_mut(&alias.owner).expect("an alias's owner");
            let mut comments = vec![alias.comment];
            comments.extend(found.comments);
            class
                .methods
                .entry((alias.singleton, alias.new.clone()))
                .or_insert(Method {
                    name: alias.new,
                    singleton: alias.singleton,
                    visibility: if found.visibility == Vis::Private {
                        Vis::Private
                    } else {
                        alias.visibility
                    },
                    overloads: found.overloads,
                    comments,
                    library: alias.library,
                });
        }
        env
    }

    /// `Foo.new` for each class that writes its own `initialize`, taking its
    /// parameters and returning a Foo, as RBS derives it.
    pub(crate) fn derive_new(&mut self) {
        for class in self.classes.values_mut() {
            if class.kind != Kind::Class || class.methods.contains_key(&(true, "new".to_string())) {
                continue;
            }
            let Some(initialize) = class.methods.get(&(false, "initialize".to_string())) else {
                continue;
            };
            let returns = Returns::Class(class.name.clone());
            let new = Method {
                name: "new".to_string(),
                singleton: true,
                visibility: Vis::Public,
                overloads: initialize
                    .overloads
                    .iter()
                    .map(|o| Overload {
                        returns: returns.clone(),
                        ..o.clone()
                    })
                    .collect(),
                comments: initialize.comments.clone(),
                library: initialize.library.clone(),
            };
            class.methods.insert((true, "new".to_string()), new);
        }
    }

    /// A method as Ruby would find it from `owner`: its own, then its
    /// mixins', then its superclass's.
    fn find(
        &self,
        owner: &str,
        singleton: bool,
        name: &str,
        seen: &mut HashSet<String>,
    ) -> Option<Method> {
        if !seen.insert(format!("{owner}{singleton}")) {
            return None;
        }
        let class = self.classes.get(owner)?;
        if let Some(method) = class.methods.get(&(singleton, name.to_string())) {
            return Some(method.clone());
        }
        let side = if singleton {
            Mixin::Extend
        } else {
            Mixin::Include
        };
        for (kind, target) in class.mixins.iter().rev() {
            if (*kind == side || *kind == Mixin::Prepend && !singleton)
                && let Some(found) = self.find(target, false, name, seen)
            {
                return Some(found);
            }
        }
        if let Some(found) = class
            .superclass
            .as_ref()
            .and_then(|superclass| self.find(superclass, singleton, name, seen))
        {
            return Some(found);
        }
        // An alias in `Kernel` may name `BasicObject`'s `__id__`: what the
        // module is mixed into.
        class
            .self_types
            .iter()
            .find_map(|target| self.find(target, singleton, name, seen))
    }
}

fn collect<'a>(
    decl: &'a Decl,
    context: &[String],
    library: &'a Library,
    out: &mut Vec<Scoped<'a>>,
) {
    let name = match decl.name.strip_prefix("::") {
        Some(absolute) => absolute.to_string(),
        None => match context.last() {
            Some(outer) => format!("{outer}::{}", decl.name),
            None => decl.name.clone(),
        },
    };
    out.push(Scoped {
        decl,
        name: name.clone(),
        context: context.to_vec(),
        library,
    });
    let mut inner = context.to_vec();
    inner.push(name);
    for member in &decl.members {
        if let Member::Decl(child) = member {
            collect(child, &inner, library, out);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn add_method(
    class: &mut Class,
    name: &str,
    singleton: bool,
    visibility: Vis,
    overloads: Vec<Overload>,
    overloading: bool,
    comment: &str,
    library: &Library,
) {
    let key = (singleton, name.to_string());
    // Implicitly private in Ruby whatever the section says.
    let visibility = match name {
        "initialize"
        | "initialize_copy"
        | "initialize_clone"
        | "initialize_dup"
        | "respond_to_missing?"
            if !singleton =>
        {
            Vis::Private
        }
        _ => visibility,
    };
    match class.methods.get_mut(&key) {
        Some(existing) if overloading => {
            // `| ...`: these come first, then the ones already declared.
            let mut merged = overloads;
            merged.append(&mut existing.overloads);
            existing.overloads = merged;
            existing.comments.push(comment.to_string());
        }
        // A second declaration without `...` is an error RBS itself reports;
        // the first one stands.
        Some(_) => {}
        None => {
            class.methods.insert(
                key,
                Method {
                    name: name.to_string(),
                    singleton,
                    visibility,
                    overloads,
                    comments: vec![comment.to_string()],
                    library: library.clone(),
                },
            );
        }
    }
}

/// `RBS::Unnamed::Random_Formatter`: a module RBS names for want of a Ruby
/// name, whose methods belong to what includes it.
pub(crate) fn is_unnamed(name: &str) -> bool {
    name.starts_with("RBS::Unnamed::")
}

/// A name as RBS resolves it: `::X` from the top; otherwise the innermost
/// scope around it that has one, then the top level. A name nothing
/// declares is kept as written.
fn resolve(written: &str, context: &[String], declared: &HashSet<String>) -> String {
    if let Some(absolute) = written.strip_prefix("::") {
        return absolute.to_string();
    }
    let head = written.split("::").next().unwrap_or(written);
    for scope in context.iter().rev() {
        if declared.contains(&format!("{scope}::{head}")) {
            return format!("{scope}::{written}");
        }
    }
    written.to_string()
}

/// What a return type names, if it is one class: not a type variable of the
/// class or the overload, not an interface or an alias.
fn returns(
    ty: &Ty,
    class_params: &[String],
    method_params: &[String],
    context: &[String],
    declared: &HashSet<String>,
) -> Returns {
    let Ty::Name { path, .. } = ty else {
        return Returns::Other;
    };
    let last = path.rsplit("::").next().unwrap_or(path);
    if !last.starts_with(|c: char| c.is_ascii_uppercase()) {
        return Returns::Other;
    }
    if !path.contains("::") && (class_params.contains(path) || method_params.contains(path)) {
        return Returns::Other;
    }
    Returns::Class(resolve(path, context, declared))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(sources: &[(&str, &str)]) -> Env {
        let sources: Vec<Source> = sources
            .iter()
            .map(|(library, text)| Source {
                library: (!library.is_empty()).then(|| library.to_string()),
                parsed: parse::parse(text),
            })
            .collect();
        Env::build(&sources)
    }

    fn returns_of(env: &Env, owner: &str, singleton: bool, name: &str) -> Vec<Returns> {
        env.classes[owner].methods[&(singleton, name.to_string())]
            .overloads
            .iter()
            .map(|o| o.returns.clone())
            .collect()
    }

    #[test]
    fn a_return_resolves_through_the_scopes_it_is_written_in() {
        let env = env(&[(
            "",
            "module Net\n  class HTTP\n    def request: () -> HTTPResponse\n    def body: () -> String\n  end\n  class HTTPResponse\n  end\nend\nclass String\nend\n",
        )]);
        assert_eq!(
            returns_of(&env, "Net::HTTP", false, "request"),
            [Returns::Class("Net::HTTPResponse".into())]
        );
        assert_eq!(
            returns_of(&env, "Net::HTTP", false, "body"),
            [Returns::Class("String".into())]
        );
    }

    #[test]
    fn a_type_variable_is_no_class() {
        let env = env(&[(
            "",
            "class Array[unchecked out Elem]\n  def first: () -> Elem\n  def sample: [T] (T) -> T\n  def to_a: () -> Array[Elem]\nend\n",
        )]);
        assert_eq!(returns_of(&env, "Array", false, "first"), [Returns::Other]);
        assert_eq!(returns_of(&env, "Array", false, "sample"), [Returns::Other]);
        assert_eq!(
            returns_of(&env, "Array", false, "to_a"),
            [Returns::Class("Array".into())]
        );
    }

    #[test]
    fn a_module_function_is_a_private_instance_and_a_public_singleton_method() {
        let env = env(&[(
            "",
            "module Kernel\n  def self?.puts: (*untyped) -> nil\nend\n",
        )]);
        let kernel = &env.classes["Kernel"];
        assert_eq!(
            kernel.methods[&(false, "puts".into())].visibility,
            Vis::Private
        );
        assert_eq!(
            kernel.methods[&(true, "puts".into())].visibility,
            Vis::Public
        );
    }

    #[test]
    fn a_reopening_adds_members_and_an_overloading_def_adds_overloads() {
        let env = env(&[
            (
                "",
                "class Time\n  def to_s: () -> String\nend\nclass String\nend\n",
            ),
            (
                "time",
                "class Time\n  def self.parse: (String) -> Time\n  def to_s: (Integer) -> Integer | ...\nend\nclass Integer\nend\n",
            ),
        ]);
        let time = &env.classes["Time"];
        assert_eq!(time.libraries, [None, Some("time".to_string())]);
        assert_eq!(
            time.methods[&(true, "parse".into())].library.as_deref(),
            Some("time")
        );
        let to_s = &time.methods[&(false, "to_s".into())];
        assert_eq!(to_s.library, None, "the method is core's, extended");
        assert_eq!(to_s.overloads.len(), 2);
    }

    #[test]
    fn an_alias_takes_the_method_it_names_even_an_ancestors() {
        let env = env(&[(
            "",
            "class Base\n  def length: () -> Integer\nend\nclass Integer\nend\nclass List < Base\n  alias size length\n  alias self.build self.new\nend\n",
        )]);
        assert_eq!(
            returns_of(&env, "List", false, "size"),
            [Returns::Class("Integer".into())]
        );
        assert!(
            !env.classes["List"]
                .methods
                .contains_key(&(true, "build".into()))
        );
    }

    #[test]
    fn an_alias_in_a_module_may_name_what_it_is_mixed_into() {
        let env = env(&[(
            "",
            "class BasicObject
  def __id__: () -> Integer
end
class Integer
end
module Kernel : BasicObject
  alias object_id __id__
end
",
        )]);
        assert_eq!(
            returns_of(&env, "Kernel", false, "object_id"),
            [Returns::Class("Integer".into())]
        );
    }

    #[test]
    fn a_class_derived_from_an_unnamed_one_has_its_members_and_superclass() {
        let env = env(&[(
            "",
            "module RBS
  module Unnamed
    class Random_Base
      include Random_Formatter
      def rand: () -> Float
    end
    module Random_Formatter
      def hex: () -> String
    end
  end
end
class Float
end
class String
end
class Random < RBS::Unnamed::Random_Base
end
",
        )]);
        let random = &env.classes["Random"];
        assert_eq!(random.superclass, None);
        assert!(random.methods.contains_key(&(false, "rand".into())));
        assert!(random.methods.contains_key(&(false, "hex".into())));
    }

    #[test]
    fn an_unnamed_modules_methods_are_its_includers() {
        let env = env(&[(
            "",
            "module RBS\n  module Unnamed\n    module Random_Formatter\n      def hex: (?Integer?) -> String\n    end\n  end\nend\nclass String\nend\nmodule Random::Formatter\n  include RBS::Unnamed::Random_Formatter\nend\n",
        )]);
        assert_eq!(
            returns_of(&env, "Random::Formatter", false, "hex"),
            [Returns::Class("String".into())]
        );
    }
}
