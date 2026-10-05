//! `--dead` (DEC-038): definitions in scope that nothing appears to use.
//! Methods are weighed here; an example group's members in `members`, and
//! classes, modules and constants in `dead_consts`.

use super::*;

/// Definitions in scope that nothing appears to use (DEC-038).
///
/// Two passes, because the cheap one settles most of it. A name with hundreds
/// of call sites is not a candidate and must not cost a receiver-narrowed
/// search to establish that; the few that survive get the expensive question
/// asked properly.
///
/// Scope is the argument, evidence is the **whole checkout** — a method used
/// once from outside the scope is not a candidate, and a scope-local search
/// would say it is. Not the whole store: what else is indexed must not change
/// the answer (DEC-074). Scopes in two checkouts are each weighed against
/// their own.
pub(super) fn cmd_dead(out: Output, paths: &[PathBuf]) -> anyhow::Result<ExitCode> {
    let mut checkouts: Vec<(PathBuf, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        let root = named_checkout(path)?;
        match checkouts.iter_mut().find(|(known, _)| *known == root) {
            Some((_, scoped)) => scoped.push(path.clone()),
            None => checkouts.push((root, vec![path.clone()])),
        }
    }
    let store = open_store()?;
    for (root, _) in &checkouts {
        if let Some(code) = autoindex::ensure(out, &store, root, Need::Whole)? {
            return Ok(code);
        }
        if !store.has_checkout(&root.to_string_lossy())? {
            return not_indexed(out, root, &store);
        }
    }
    // Every candidate is a claim that no caller exists anywhere, and a
    // partial index cannot make it: wait for the rest (DEC-320).
    for (root, _) in &checkouts {
        let root = root.to_string_lossy();
        if let Some(warming) = store.warming(&root)? {
            crate::usage::outcome(Outcome::NotIndexed);
            let reason = format!(
                "the index has read {} of {} files, and a caller may be in one not read yet",
                warming.read, warming.of
            );
            match out {
                Output::Text => println!("--dead lists nothing until the index ends: {reason}"),
                _ => emit_json(
                    out,
                    &serde_json::json!({
                        "status": "warming",
                        "repo": root,
                        "reason": reason,
                        "warming": warming_note(&root, &warming),
                        "candidates": [],
                    }),
                )?,
            }
            return Ok(ExitCode::from(2));
        }
    }
    // Across checkouts no one root is "here", so text writes every path
    // whole rather than relative to whichever scope came first.
    if let Some((first, _)) = checkouts.first() {
        answering_in(&store, &first.to_string_lossy());
    }
    if checkouts.len() > 1 {
        TEXT_ABSOLUTE.store(true, std::sync::atomic::Ordering::Relaxed);
    }

    let mut rows: Vec<serde_json::Value> = Vec::new();
    let mut scope = 0;
    for (root, scoped) in &checkouts {
        scope += dead_in(&store, root, scoped, &mut rows)?;
    }
    note_candidate_callers(&mut rows);

    let found = !rows.is_empty();
    let summary = dead_summary(&rows);
    if out != Output::Text {
        let answer = serde_json::json!({ "scope": scope, "summary": summary, "candidates": null });
        emit_listing(out, answer, "candidates", &rows)?;
        return Ok(exit_on(found));
    }
    // Methods first, then example groups' own, then the classes, modules and
    // constants, each apart.
    let family = |row: &serde_json::Value| match row["kind"].as_str() {
        _ if row.get("group").is_some() => 1,
        Some("shared_group") => 1,
        Some("method") => 0,
        _ => 2,
    };
    for (at, row) in rows.iter().enumerate() {
        if at > 0 && family(row) != family(&rows[at - 1]) {
            println!();
        }
        let visibility = match row["visibility"].as_str() {
            Some("public") | None => String::new(),
            Some(other) => format!(" ({other})"),
        };
        println!(
            "{:<16} {}  {}{visibility}  — {}{}",
            row["tier"].as_str().unwrap_or_default(),
            at_line(row),
            dead_name(row),
            row["reason"].as_str().unwrap_or_default(),
            match row["caveat"].as_str().unwrap_or_default() {
                "" if row["confidence"] == "lower" => "   (lower confidence)".to_string(),
                "" => String::new(),
                why => format!("   (lower confidence: {why})"),
            }
        );
    }
    if !found {
        println!("no candidates in {scope} file(s)");
        return Ok(exit_on(found));
    }
    let tiers: Vec<String> = DEAD_TIERS
        .iter()
        .map(|tier| (tier, summary["tiers"][tier].as_u64().unwrap_or(0)))
        .filter(|(_, n)| *n > 0)
        .map(|(tier, n)| format!("{n} {tier}"))
        .collect();
    let constants = rows.iter().filter(|row| family(row) == 2).count();
    let members = rows.iter().filter(|row| family(row) == 1).count();
    let of_them: Vec<String> = [
        (
            members,
            "example groups' lets, subjects, defs or shared groups",
        ),
        (constants, "classes, modules or constants"),
    ]
    .iter()
    .filter(|(n, _)| *n > 0)
    .map(|(n, what)| format!("{n} of them {what}"))
    .collect();
    println!(
        "\n{} candidates in {scope} file(s): {} ({} clear, {} lower){}",
        rows.len(),
        tiers.join(", "),
        summary["confidence"]["clear"],
        summary["confidence"]["lower"],
        match of_them.is_empty() {
            true => String::new(),
            false => format!("; {}", of_them.join(", ")),
        },
    );
    Ok(exit_on(found))
}

/// The class Action Mailer's mailers inherit, whose class side runs an action.
const MAILER: &str = "ActionMailer::Base";

/// `--dead`'s tiers, from the least evidence of use to the most.
const DEAD_TIERS: [&str; 7] = [
    "unreferenced",
    "shadowed",
    "test-only",
    "override",
    "convention-only",
    "super-only",
    "single-caller",
];

/// How many candidates in each tier, and at each confidence. Every tier is
/// present, so a script reads a zero rather than a missing key.
fn dead_summary(rows: &[serde_json::Value]) -> serde_json::Value {
    let count = |key: &str, value: &str| rows.iter().filter(|row| row[key] == value).count();
    let tiers: serde_json::Map<String, serde_json::Value> = DEAD_TIERS
        .iter()
        .map(|tier| (tier.to_string(), count("tier", tier).into()))
        .collect();
    let kinds: serde_json::Map<String, serde_json::Value> = [
        "method",
        "let",
        "subject",
        "shared_group",
        "class",
        "module",
        "constant",
    ]
    .iter()
    .map(|kind| (kind.to_string(), count("kind", kind).into()))
    .collect();
    serde_json::json!({
        "candidates": rows.len(),
        "tiers": tiers,
        "kinds": kinds,
        "confidence": { "clear": count("confidence", "clear"), "lower": count("confidence", "lower") },
    })
}

/// A candidate as Ruby's documentation names it: `Widget#save`, or
/// `Widget.build` for a method on the singleton; a class, module or constant
/// by its kind and whole name (`class Admin::Widget`).
fn dead_name(row: &serde_json::Value) -> String {
    if row.get("group").is_some() {
        return members::dead_name(row);
    }
    let name = row["name"].as_str().unwrap_or_default();
    if let Some(kind @ ("class" | "module" | "constant")) = row["kind"].as_str() {
        return match row["owner"].as_str().unwrap_or_default() {
            "" => format!("{kind} {name}"),
            owner => format!("{kind} {owner}::{name}"),
        };
    }
    match row["owner"].as_str().unwrap_or_default() {
        "" => name.to_string(),
        owner if row["singleton"] == true => format!("{owner}.{name}"),
        owner => format!("{owner}#{name}"),
    }
}

/// Each method a route reaches, by its owner and name, with the route's
/// file and line: a route's controller is the class its path camelizes to,
/// matched without case or underscores (`api/v1/oauth` is `Api::V1::OAuth
/// Controller` under an acronym inflection), and an engine's own first; its
/// action is what that class's lookup of the name finds, which may be a
/// superclass's (DEC-344).
fn routed_actions(
    tree: &crate::tree::Tree,
    routes: &routes::Routes,
) -> HashMap<(String, String), routes::At> {
    let plain = |name: &str| name.replace('_', "").to_lowercase();
    let controllers: HashMap<String, String> = tree
        .declared()
        .into_iter()
        .filter(|(fqn, kind)| kind == "class" && fqn.ends_with("Controller"))
        .map(|(fqn, _)| (plain(&fqn), fqn))
        .collect();
    let mut routed = HashMap::new();
    for route in &routes.routes {
        let class = route.controllers.iter().find_map(|controller| {
            let path = format!(
                "{}controller",
                controller.split('/').collect::<Vec<_>>().join("::")
            );
            let engine = route
                .engine
                .as_ref()
                .map(|engine| format!("{engine}::{path}"));
            engine
                .iter()
                .chain(std::iter::once(&path))
                .find_map(|name| controllers.get(&plain(name)))
        });
        let Some(class) = class else {
            continue;
        };
        let Some(method) = tree.lookup(class, false, &route.action) else {
            continue;
        };
        routed
            .entry((method.owner, route.action.clone()))
            .or_insert_with(|| route.at.clone());
    }
    routed
}

/// A method `--dead` weighs, with what its own file says about it.
struct Defined {
    file: String,
    def: crate::core::Def,
    /// What lowers confidence in it, file-wide or its own.
    caveat: String,
    /// Other names its body is called by, and on which side: an `alias`
    /// of it (DEC-316), and a `module_function`'s singleton copy (DEC-361).
    aliases: Vec<(String, bool)>,
    /// The first line of its file with a symbol of its name that no rule
    /// reads as a call of it (DEC-343).
    unread_symbol: Option<u32>,
    /// Whether its body calls `super`: it overrides something (DEC-365).
    calls_super: bool,
    /// Its file's facts, for what a `def` its scope may not own is on.
    facts: std::sync::Arc<crate::core::Facts>,
}

/// `--dead` over the scopes in one checkout, weighed against that checkout:
/// pushes a row per candidate and returns how many files were in scope.
fn dead_in(
    store: &Store,
    root: &Path,
    paths: &[PathBuf],
    rows: &mut Vec<serde_json::Value>,
) -> anyhow::Result<usize> {
    use crate::resolve::refs;

    let root_str = root.to_string_lossy().into_owned();
    let files = ruby_files(paths);
    let mut defined: Vec<Defined> = Vec::new();
    // An example group's own methods, weighed by who reads them (DEC-490).
    let mut group_members: Vec<(String, crate::core::Def)> = Vec::new();
    // Each file's facts, for the members' reader to read rather than parse again.
    let mut scope_facts: Vec<(String, std::sync::Arc<crate::core::Facts>)> = Vec::new();
    // Read and parsed in parallel, weighed in order.
    let parsed: Vec<_> = files
        .par_iter()
        .filter_map(|file| {
            let raw = crate::scan::read_source(file).ok()?;
            let source = extract::ruby_source(&file.to_string_lossy(), &raw).into_owned();
            let facts = std::sync::Arc::new(extract::extract(&source));
            let symbols = extract::symbol_literals(&source);
            Some((file, source, facts, symbols))
        })
        .collect();
    for (file, source, facts, symbols) in parsed {
        scope_facts.push((file.to_string_lossy().into_owned(), facts.clone()));
        // A dynamic-dispatch marker anywhere in the file lowers confidence for
        // everything in it: these are the shapes that make "no references" a
        // weaker statement, and they are file-wide by nature.
        let mut risky = dynamic_markers(&source);
        // A string of code not read: its calls are not in the index (DEC-132).
        let unread = facts.ancestry.iter().any(|edge| {
            edge.relation == crate::core::Relation::Dynamic
                && crate::core::Maker::parse(&edge.target)
                    .by
                    .ends_with(" string")
        });
        if unread {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str("class_eval string");
        }
        let at = file.to_string_lossy().into_owned();
        // The file's symbols no rule reads as a method's name (DEC-343).
        let recorded: Vec<crate::core::Pos> = facts
            .calls
            .iter()
            .filter(|c| c.recv == crate::core::RecvShape::Symbol)
            .map(|c| c.pos)
            .collect();
        let unread_symbols: Vec<(String, u32)> = symbols
            .into_iter()
            .filter(|(_, pos, _)| !recorded.contains(pos))
            .map(|(name, pos, _)| (name, pos.line))
            .collect();
        let unread_calls = &facts.unread_calls;
        // A call of an alias runs its target's body (DEC-316), and so does a
        // call of a `module_function`'s singleton copy (DEC-361).
        let aliases_of = |def: &crate::core::Def| -> Vec<(String, bool)> {
            facts
                .defs
                .iter()
                .filter(|other| other.nesting == def.nesting)
                .filter_map(|other| match other.via.as_deref() {
                    Some("alias" | "alias_method")
                        if other.target.as_deref() == Some(def.name.as_str())
                            && other.singleton == def.singleton =>
                    {
                        Some((other.name.clone(), def.singleton))
                    }
                    Some("module_function")
                        if other.name == def.name && other.singleton && !def.singleton =>
                    {
                        Some((other.name.clone(), true))
                    }
                    _ => None,
                })
                .collect()
        };
        for def in &facts.defs {
            if def.kind != crate::core::Kind::Method {
                continue;
            }
            if crate::resolve::members::is_member(def) {
                if !members::eager(def)
                    && !crate::resolve::members::names_a_named_subject(def, &facts)
                {
                    group_members.push((at.clone(), def.clone()));
                }
                continue;
            }
            // A group's method on its class side is no member any read reaches.
            if def.is_group_member() {
                continue;
            }
            // A schema column is not dead because nothing calls it; that is a
            // fact about the database. Same for anything a macro declared —
            // deleting the method means editing the macro, which is a different
            // question than this one.
            if def.via.is_some() {
                continue;
            }
            // A string of code calls a name of its shape that it spells only
            // in part, which no call site records (DEC-163).
            let mut caveat = risky.clone();
            if let Some(shape) = unread_calls
                .iter()
                .find(|shape| crate::core::shape_matches(shape, &def.name))
            {
                if !caveat.is_empty() {
                    caveat.push_str(", ");
                }
                caveat.push_str(&format!("a string of code calls `{shape}`"));
            }
            let aliases = aliases_of(def);
            // Its own body's `:name` is a value it uses, not a way in.
            let own = def.pos.line..=def.end_line;
            let unread_symbol = unread_symbols
                .iter()
                .find(|(name, line)| *name == def.name && !own.contains(line))
                .map(|(_, line)| *line);
            let calls_super = facts
                .calls
                .iter()
                .any(|c| c.recv == crate::core::RecvShape::Super && own.contains(&c.pos.line));
            defined.push(Defined {
                file: at.clone(),
                def: def.clone(),
                caveat,
                aliases,
                unread_symbol,
                calls_super,
                facts: facts.clone(),
            });
        }
    }

    let names: Vec<String> = defined.iter().map(|d| d.def.name.clone()).collect();
    // More written calls than this and a name is plainly used.
    const PLAINLY_USED: i64 = 8;
    let written_calls = store.written_calls(&root_str, &names, PLAINLY_USED + 1)?;

    // The expensive pass, only for names the cheap one could not clear.
    let tree = build_tree(store, &root_str)?;
    let views = views::Views::read(root);
    let config = config::Config::read(root);
    let generated = generated::Generated::read(root, defined.iter().map(|d| d.file.as_str()));
    let texts = built::Texts::read(root);
    let built = built::Built::read(&texts);
    let routes = routes::Routes::read(root);
    let routed = routed_actions(&tree, &routes);
    let mut symbols = conventions::Symbols::default();
    let mut thor_blocks = conventions::ThorBlocks::default();
    let mut foreign_sends = conventions::ForeignSends::default();
    let mut parsed = Parsed::default();
    for Defined {
        file,
        def,
        caveat: risky,
        aliases,
        unread_symbol,
        calls_super,
        facts,
    } in &defined
    {
        // The class it is, not the name as written: `Helpers` inside `module
        // Alpha` is `Alpha::Helpers`, and that is what a resolved call names.
        let owner = tree
            .scope_fqn(&def.nesting)
            .or_else(|| def.nesting.first().cloned())
            .unwrap_or_default();
        let query = refs::Query {
            owner: Some(owner.clone()),
            singleton: def.singleton,
            name: def.name.clone(),
        };
        // `initialize` is written by its every subclass's `super`, which
        // says nothing about this one: its `X.new`s are what count (DEC-541).
        let written = written_calls.get(&def.name).copied().unwrap_or(0);
        if written > PLAINLY_USED && refs::constructor_of(&query).is_none() {
            continue; // not worth a narrowed search
        }
        let (mut found, mut counts) = gather_refs(
            &tree,
            store,
            root,
            &root_str,
            &query,
            Some(&owner),
            false,
            Some(&mut parsed),
            None,
        )
        .unwrap_or_default();
        for (alias, singleton) in aliases {
            let query = refs::Query {
                owner: query.owner.clone(),
                singleton: *singleton,
                name: alias.clone(),
            };
            let (more, tally) = gather_refs(
                &tree,
                store,
                root,
                &root_str,
                &query,
                Some(&owner),
                false,
                Some(&mut parsed),
                None,
            )
            .unwrap_or_default();
            found.extend(more);
            counts.add(&tally);
        }
        // Action Mailer runs an action through its class, whose
        // `method_missing` the index finds nothing behind (DEC-369).
        let constructor = refs::constructor_of(&query).is_some();
        // Ruby makes `initialize` private wherever it is written.
        let public = def.visibility.as_str() == "public" && !constructor;
        if !def.singleton && public && tree.inherits(&owner, MAILER) {
            let query = refs::Query {
                owner: query.owner.clone(),
                singleton: true,
                name: def.name.clone(),
            };
            let (all, _) = gather_refs(
                &tree,
                store,
                root,
                &root_str,
                &query,
                Some(&owner),
                true,
                Some(&mut parsed),
                None,
            )
            .unwrap_or_default();
            for mut call in all
                .into_iter()
                .filter(|r| r.ruling == Some(refs::Ruling::NoSuchMethod))
                .filter(|r| {
                    r.receiver_type
                        .as_deref()
                        .is_some_and(|class| class == owner || tree.inherits(class, &owner))
                })
            {
                call.tier = refs::Tier::Confirmed;
                call.ruling = None;
                call.why = "Action Mailer runs the action its class is sent";
                counts.confirmed += 1;
                found.push(call);
            }
        }
        // A bare call in a template that is not a view is any class's, and
        // so, for an `initialize`, are an `X.new` on a value of no known
        // class, a `super` trekr cannot place and a macro's symbol: they keep
        // it alive no more than a grep would, so they are weighed as a caveat
        // instead (DEC-541, DEC-630).
        let unplaced_sites = unplaced_caveat(&mut found, &mut counts, constructor);
        // A `def` its scope may not own: a call of its name whose receiver
        // has no such method may be on the object it is defined on (DEC-562).
        let defined_on = crate::resolve::defined_on(&tree, facts, def, file);
        // An `initialize` in a block whose `self` is not known is an
        // anonymous class's (`Class.new do`), built through whatever holds
        // it: nothing typed can say it is unused.
        if constructor && defined_on.is_some() {
            continue;
        }
        if let Some(on) = &defined_on {
            let (all, _) = gather_refs(
                &tree,
                store,
                root,
                &root_str,
                &query,
                Some(&owner),
                true,
                Some(&mut parsed),
                None,
            )
            .unwrap_or_default();
            let may_be = |class: &str| match on {
                DefinedOn::Object(of) => class == of || tree.inherits(of, class),
                DefinedOn::Unknown => true,
            };
            for mut call in all
                .into_iter()
                .filter(|r| r.ruling == Some(refs::Ruling::NoSuchMethod))
                .filter(|r| r.receiver_type.as_deref().is_none_or(may_be))
            {
                call.tier = refs::Tier::Possible;
                call.ruling = None;
                call.why = "the method is defined on an object the receiver may be";
                counts.possible += 1;
                found.push(call);
            }
        }
        let live = refs::liveness(&found, &counts);
        let Some(tier) = live.tier else { continue };
        // An `initialize` one `X.new` reaches is how a class is built, not a
        // method to inline; one only subclasses' `super`s reach belongs to an
        // abstract class. Only one nothing reaches is a candidate.
        if constructor && tier != "unreferenced" {
            continue;
        }
        // Whoever calls the method this overrides may run it instead, and
        // that is often a framework the checkout never names (DEC-121) —
        // but `Class#new` runs a class's own `initialize`, never one it
        // overrides, so for one that is a fact and not a way in. One object's
        // own method replaces its class's for that object (DEC-562).
        let overrides = match &defined_on {
            Some(DefinedOn::Object(of)) => tree
                .lookup(of, false, &def.name)
                .map(|found| vec![format!("{}#{}", public_name(&found.owner), def.name)])
                .unwrap_or_default(),
            _ => crate::resolve::overridden(&tree, def, file),
        };
        let tier = match tier {
            "unreferenced" if !overrides.is_empty() && !constructor => "override",
            tier => tier,
        };
        // A controller's public action a route reaches is reached by
        // convention, as a symbol handed to a macro is (DEC-344); so is a
        // concern's, which is the action of the controllers that include it.
        let controller = |class: &str| class.ends_with("Controller");
        let action = !def.singleton
            && public
            && (controller(&owner) || tree.includers_of(&owner).iter().any(|c| controller(c)));
        let route = action
            .then(|| routed.get(&(owner.clone(), def.name.clone())))
            .flatten();
        // A library that calls it by a name it builds (DEC-362, DEC-371).
        let convention = (matches!(tier, "unreferenced" | "override") && !def.singleton)
            .then(|| {
                conventions::serializer_include(&tree, &owner, &def.name, &mut symbols).or_else(
                    || {
                        let at = (file.as_str(), def.pos.line);
                        conventions::thor_command(&tree, &owner, public, at, &mut thor_blocks)
                            .or_else(|| {
                                conventions::pundit_predicate(
                                    &tree, &owner, &def.name, public, &root_str,
                                )
                            })
                    },
                )
            })
            .flatten();
        let tier = match tier {
            "unreferenced" | "override" if route.is_some() || convention.is_some() => {
                "convention-only"
            }
            tier => tier,
        };
        // The one written call a single caller has: whether it certainly
        // reaches this method is the difference between inlining it and
        // checking an untyped receiver first.
        let single_call = (tier == "single-caller")
            .then(|| found.iter().find(|r| refs::is_written_call(r)))
            .flatten();
        let caller = single_call.map(|r| {
            serde_json::json!({
                "path": format!("{root_str}/{}", r.path),
                "line": r.line,
                "col": r.col,
                "tier": r.tier,
            })
        });
        // Its only evidence of use is a call that may be another method's:
        // that is weaker than a clear single caller, and says why.
        let mut risky = risky.clone();
        // Nothing seen constructs it, and a class is constructed wherever it
        // is handed, by name: graded lower as a class is (DEC-450, DEC-541).
        if constructor {
            let ancestors = tree.ancestors(&owner);
            let exception = |name: &String| matches!(name.as_str(), "Exception" | "StandardError");
            let how = if ancestors
                .chain
                .iter()
                .chain(&ancestors.unresolved)
                .any(exception)
            {
                "an exception class, which `raise` constructs from its name"
            } else if ancestors.chain.iter().any(|a| a == "Singleton") {
                "a Singleton, which `instance` constructs"
            } else if tree.kind_of(&owner) == Some("module") {
                "a module's, which runs when a class that mixes it in is constructed"
            } else {
                "a class may be constructed by whatever it is handed to — a library, a registry"
            };
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(how);
        }
        if let Some(unplaced) = &unplaced_sites {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(unplaced);
        }
        if caller.as_ref().is_some_and(|c| c["tier"] == "possible") {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str("untyped caller");
        }
        if tier != "override" && !constructor && !overrides.is_empty() {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!("overrides {}", overrides.join(", ")));
        }
        let elsewhere = match (&defined_on, &def.unsettled) {
            (Some(DefinedOn::Object(of)), _) => Some(format!("defined on one `{of}` object")),
            (Some(DefinedOn::Unknown), Some(crate::core::Unsettled::Object { local, .. })) => Some(
                format!("defined on the object `{local}` holds, which trekr cannot type"),
            ),
            (Some(DefinedOn::Unknown), _) => {
                Some("defined in a block whose `self` trekr cannot pin down".to_string())
            }
            (None, _) => None,
        };
        if let Some(elsewhere) = elsewhere {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&elsewhere);
        }
        // An ancestor not indexed may call it, and a `super` says one is
        // there (DEC-365).
        if tier == "unreferenced" {
            // A name the tree knows is no unseen ancestor: two declarations
            // of the owner (`User = Data.define` in a script) leave the
            // other's superclass unresolved, though it is indexed.
            let unseen: Vec<String> = tree
                .ancestors(&owner)
                .unresolved
                .iter()
                .filter(|name| !tree.is_known(name))
                .cloned()
                .collect();
            if !unseen.is_empty() {
                if !risky.is_empty() {
                    risky.push_str(", ");
                }
                risky.push_str(&format!(
                    "an ancestor trekr has not indexed ({}) may call it",
                    unseen.join(", ")
                ));
            }
            // A `super` that lands on an indexed method says nothing unseen.
            if *calls_super && overrides.is_empty() {
                if !risky.is_empty() {
                    risky.push_str(", ");
                }
                risky.push_str("it calls `super`, so it overrides a method trekr has not indexed");
            }
            if public
                && !def.singleton
                && let Some((path, line)) = foreign_sends.of(&tree, &owner)
            {
                if !risky.is_empty() {
                    risky.push_str(", ");
                }
                risky.push_str(&format!(
                    "an ancestor outside the checkout sends `self` a name it computes ({path}:{line})"
                ));
            }
        }
        // A call of its name ruled out on a `self` trekr cannot name may be
        // its: Rails mixes every app/helpers module into one view context, so
        // a helper's call with no receiver may reach another's (DEC-368), and
        // in a module nothing indexed includes `self` is whatever instance
        // runs it — a condition lambda the module hands a macro (DEC-380).
        let helper = |path: &str| path.split('/').any(|dir| dir == "helpers");
        let mut unplaced = None;
        // The overrides a call of its name lands on, in a subclass: each call
        // that would run it runs one of them instead (DEC-491). Not an
        // `initialize`'s: a subclass's own calls `super`, and nothing calls
        // one with no receiver.
        let mut shadowing: Vec<String> = Vec::new();
        if tier == "unreferenced" && !def.singleton && !constructor && counts.excluded > 0 {
            let (all, _) = gather_refs(
                &tree,
                store,
                root,
                &root_str,
                &query,
                Some(&owner),
                true,
                Some(&mut parsed),
                None,
            )
            .unwrap_or_default();
            for call in all
                .iter()
                .filter(|r| r.ruling == Some(refs::Ruling::DifferentOwner))
            {
                if let Some(landed) = call.owner.as_deref()
                    && tree.inherits(landed, &owner)
                {
                    let at = format!("{landed}#{}", def.name);
                    if !shadowing.contains(&at) {
                        shadowing.push(at);
                    }
                }
            }
            let helper_caller = all.iter().find(|r| {
                helper(file)
                    && r.tier == refs::Tier::Excluded
                    && r.receiver == "implicit"
                    && r.path.starts_with("app/helpers/")
                    && !file.ends_with(&r.path)
            });
            // An instance's `self`: a module's own `def self.` knows its.
            let on_instance = |r: &refs::Reference| {
                parsed
                    .facts
                    .get(&r.path)
                    .and_then(Option::as_ref)
                    .and_then(|facts| {
                        facts
                            .calls
                            .iter()
                            .find(|c| c.pos.line == r.line && c.pos.col == r.col)
                    })
                    .is_some_and(|call| !call.singleton)
            };
            let unknown_self = || {
                all.iter().find(|r| {
                    r.ruling == Some(refs::Ruling::NoSuchMethod)
                        && matches!(r.receiver, "implicit" | "self")
                        && r.receiver_type.as_deref().is_some_and(|module| {
                            tree.kind_of(module) == Some("module")
                                && tree.includers_of(module).is_empty()
                        })
                        && on_instance(r)
                })
            };
            if let Some(caller) = helper_caller {
                unplaced = Some(format!(
                    "called with no receiver in {}:{}, a helper Rails mixes into the same views",
                    caller.path, caller.line
                ));
            } else if let Some(caller) = unknown_self() {
                unplaced = Some(format!(
                    "called with no receiver at {}:{}, in a module nothing indexed includes, \
                     so its `self` is not known",
                    caller.path, caller.line
                ));
            }
        }
        if let Some(unplaced) = &unplaced {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(unplaced);
        }
        // A gem of the bundle calls the name on an object it is handed: an
        // instance's method, since such a call's receiver is a value (DEC-367).
        if tier == "unreferenced"
            && !def.singleton
            && let Some((gem, n)) = store.bundle_calls(&root_str, &def.name)?
        {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            let gem = Path::new(&gem)
                .file_name()
                .map_or(gem.clone(), |name| name.to_string_lossy().into_owned());
            risky.push_str(&format!(
                "a gem in the bundle calls a method of this name ({gem}, {n} {})",
                if n == 1 { "site" } else { "sites" }
            ));
        }
        // Callers `--dead` cannot see, named where it can say which (DEC-315).
        if let Some(caller) = refs::protocol_hook(&def.name, def.singleton) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!("a hook {caller} calls by name"));
        }
        if let Some(template) = views.naming(&def.name) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!("named in a view ({template}), which is not read"));
        }
        if let Some(at) = config.naming(&def.name) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!("named in config ({at}), which is not read"));
        }
        if let Some(how) = generated.why(file) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!(
                "in generated code ({how}), which its runtime may call generically"
            ));
        }
        if !def.singleton && conventions::assigned_writer(&tree, &owner, &def.name, public) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str("a writer Active Model's `assign_attributes` calls by name");
        }
        if let Some(why) = built.reaching(&owner, &def.name) {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&why);
        }
        // A symbol in its own file that no rule reads as its name may still
        // be how it is reached: say so, rather than that no symbol names it.
        let unread_symbol = unread_symbol.filter(|_| matches!(tier, "unreferenced" | "override"));
        // An action no route read reaches may be reached by one not read.
        if action && route.is_none() && matches!(tier, "unreferenced" | "override") {
            let unread = match routes.unread.first() {
                _ if routes.files == 0 => Some("no routes file read".to_string()),
                Some(((path, line), why)) => Some(format!("{why} at {path}:{line}")),
                None => None,
            };
            if let Some(unread) = unread {
                if !risky.is_empty() {
                    risky.push_str(", ");
                }
                risky.push_str(&format!("a public action routes may reach ({unread})"));
            }
        }
        if let Some(line) = unread_symbol {
            if !risky.is_empty() {
                risky.push_str(", ");
            }
            risky.push_str(&format!(
                "`:{}` at line {line} is not read as a call",
                def.name
            ));
        }
        let shadowed = tier == "unreferenced" && unplaced.is_none() && !shadowing.is_empty();
        let tier = if shadowed { "shadowed" } else { tier };
        let reason = match (tier, &caller) {
            ("shadowed", _) => format!(
                "no call reaches it: every call of its name lands on an override in a subclass, {}",
                shadowing.join(", ")
            ),
            ("unreferenced", _) if unplaced.is_some() => {
                "no call trekr can place on it, nor a symbol or `super`, names it".to_string()
            }
            ("unreferenced", _) if unread_symbol.is_some() => {
                "no call or `super` names it, nor a symbol trekr reads as a call".to_string()
            }
            ("unreferenced", _) if action && routes.files > 0 => {
                "no call, symbol, `super` or route names it".to_string()
            }
            ("unreferenced", _) => "no call, symbol or `super` names it".to_string(),
            ("override", _) => format!(
                "no call names it, but it overrides {}, so a call of that may run it",
                overrides.join(", ")
            ),
            ("convention-only", _) if convention.is_some() => {
                convention.as_ref().expect("checked").reason.clone()
            }
            ("convention-only", _) => match (live.by_symbol, route) {
                (0, Some((path, line))) => format!("named only by a route, at {path}:{line}"),
                (n, Some((path, line))) => format!(
                    "named only by a symbol handed to a macro ({n}) and a route, at {path}:{line}"
                ),
                (n, None) => format!("named only by a symbol handed to a macro ({n})"),
            },
            ("super-only", _) => format!(
                "reached only by `super` from {}",
                live.super_from.join(", ")
            ),
            (_, Some(caller)) if caller["tier"] == "confirmed" => {
                format!("one call, at {}", at_line(caller))
            }
            // A call found by where it runs says how (DEC-499).
            (_, Some(caller)) if let Some(r) = single_call.filter(|r| r.from.is_some()) => {
                format!("one possible call, at {}: {}", at_line(caller), r.why)
            }
            (_, Some(caller)) => format!(
                "one possible call, at {}: its receiver is untyped",
                at_line(caller)
            ),
            _ => String::new(),
        };
        let mut row = serde_json::json!({
            "kind": "method",
            "name": def.name,
            "owner": owner,
            "singleton": def.singleton,
            // Whether deleting it could break a caller outside the checkout.
            "visibility": def.visibility.as_str(),
            "path": file,
            "line": def.pos.line,
            "col": def.pos.col,
            "end_line": def.end_line,
            "tier": tier,
            "confirmed": counts.confirmed,
            "possible": counts.possible,
            "symbol_refs": live.by_symbol,
            "super_refs": live.by_super,
            "super_from": live.super_from,
            "mentions_by_name": written,
            "overrides": overrides,
            "overridden_by": shadowing,
            "confidence": if risky.is_empty() && tier != "override" { "clear" } else { "lower" },
            "caveat": risky,
            "reason": reason,
        });
        if let Some(caller) = caller {
            row["caller"] = caller;
        }
        // A method of one object is that object's class's, as a caller sees it.
        if let Some(DefinedOn::Object(of)) = &defined_on {
            row["owner"] = public_name(of).into();
            row["singleton"] = false.into();
        }
        if let Some((path, line)) = route {
            row["route"] = serde_json::json!({ "path": path, "line": line });
        }
        if let Some(convention) = convention {
            row["convention"] = serde_json::json!({ "by": convention.by });
            if let Some((path, line)) = convention.at {
                row["convention"]["path"] = path.into();
                row["convention"]["line"] = line.into();
            }
        }
        rows.push(row);
    }
    let checkout_files = crate::query::members::CheckoutFiles::new(store, root, &root_str);
    let relative = |file: &str| {
        Path::new(file)
            .strip_prefix(root)
            .map_or(file.to_string(), |p| p.to_string_lossy().into_owned())
    };
    for (file, facts) in scope_facts {
        checkout_files.hold(&relative(&file), facts);
    }
    let scope: Vec<(String, String)> = files
        .iter()
        .map(|file| file.to_string_lossy().into_owned())
        .map(|file| (file.clone(), relative(&file)))
        .collect();
    // A shared group nothing includes goes with its own members (DEC-493).
    let mut shared_rows = Vec::new();
    let unincluded = members::dead_shared_groups(&checkout_files, &scope, &mut shared_rows);
    let context = crate::resolve::members::Context::new(&tree, &checkout_files);
    for (file, def) in &group_members {
        let relative = relative(file);
        let in_unincluded = !unincluded.is_empty()
            && crate::resolve::members::Files::facts(&checkout_files, &relative).is_some_and(
                |facts| {
                    crate::resolve::members::shared_group_of(&relative, def, &facts)
                        .is_some_and(|module| unincluded.contains(&module))
                },
            );
        if in_unincluded {
            continue;
        }
        if let Some(row) = members::dead_row(&context, file, &relative, def) {
            rows.push(row);
        }
    }
    rows.extend(shared_rows);
    let sources = dead_consts::Sources {
        routes: &routes,
        views: &views,
        config: &config,
        texts: &texts,
    };
    dead_consts::dead_constants(&tree, store, root, &files, sources, rows)?;
    Ok(files.len())
}

/// One pass does not cascade: a method whose only caller is itself a
/// candidate is `single-caller`, not `unreferenced`. Say so on the row,
/// where the next question is asked.
fn note_candidate_callers(rows: &mut [serde_json::Value]) {
    let spans: Vec<(String, u64, u64, String)> = rows
        .iter()
        .map(|row| {
            (
                row["path"].as_str().unwrap_or_default().to_string(),
                row["line"].as_u64().unwrap_or(0),
                row["end_line"].as_u64().unwrap_or(0),
                row["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    for row in rows.iter_mut() {
        let caller = &row["caller"];
        let (Some(path), Some(line)) = (caller["path"].as_str(), caller["line"].as_u64()) else {
            continue;
        };
        let within = spans
            .iter()
            .find(|(p, start, end, _)| p == path && (*start..=*end).contains(&line));
        if let Some((_, _, _, name)) = within {
            let reason = format!(
                "{}; its caller, {name}, is itself a candidate",
                row["reason"].as_str().unwrap_or_default()
            );
            row["reason"] = reason.into();
        }
    }
}

/// `path:line` of a located JSON object, as text shows a path.
fn at_line(site: &serde_json::Value) -> String {
    format!(
        "{}:{}",
        shown(site["path"].as_str().unwrap_or_default()),
        site["line"]
    )
}

/// Ruby files under these paths, each once: the same file named twice — a
/// path repeated, a directory and a file in it, a symlink and its target —
/// is one file in scope, and would otherwise be two candidates.
fn ruby_files(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    ruby_files_under(paths)
        .into_iter()
        .map(|file| std::fs::canonicalize(&file).unwrap_or(file))
        .filter(|file| seen.insert(file.clone()))
        .collect()
}

/// Ruby files under these paths, following directories one level of recursion.
fn ruby_files_under(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for path in paths {
        if path.is_file() {
            found.push(path.clone());
            continue;
        }
        let Ok(walk) = std::fs::read_dir(path) else {
            continue;
        };
        for entry in walk.flatten() {
            let child = entry.path();
            if child.is_dir() {
                found.extend(ruby_files_under(&[child]));
            } else if child.extension().is_some_and(|e| e == "rb") {
                found.push(child);
            }
        }
    }
    found
}

/// Shapes that make "no references found" a weaker statement, named so the
/// answer can say which one it saw rather than hedging in general.
fn dynamic_markers(source: &[u8]) -> String {
    let text = String::from_utf8_lossy(source);
    let mut seen: Vec<&str> = Vec::new();
    for marker in [
        "send(",
        "public_send(",
        "method_missing",
        "define_method",
        "const_get",
    ] {
        if text.contains(marker) {
            seen.push(marker.trim_end_matches('('));
        }
    }
    seen.join(", ")
}

/// Takes the sites that name no class they would run in out of a method's
/// evidence, and says what they were: `None` when there were none. Any
/// method's bare calls in a template that is not a view; an `initialize`'s
/// untyped `new`s, unplaced `super`s and macro symbols too (DEC-541).
fn unplaced_caveat(
    found: &mut Vec<crate::resolve::refs::Reference>,
    counts: &mut crate::resolve::refs::Counts,
    constructor: bool,
) -> Option<String> {
    use crate::resolve::refs::Unplaced;
    // What each kind is called, for one site and for several.
    let kinds: &[(Unplaced, &str, &str)] = if constructor {
        &[
            (Unplaced::New, "`new` on untyped receivers may reach it", ""),
            (
                Unplaced::Super,
                "`super` whose landing trekr cannot place",
                "`super`s whose landing trekr cannot place",
            ),
            (
                Unplaced::Symbol,
                "symbol handed to a macro",
                "symbols handed to a macro",
            ),
        ]
    } else {
        &[(
            Unplaced::Template,
            "bare call in a template whose `self` trekr cannot name",
            "bare calls in templates whose `self` trekr cannot name",
        )]
    };
    let mut parts = Vec::new();
    for &(kind, one, many) in kinds {
        let sites: Vec<_> = found
            .iter()
            .filter(|r| r.unplaced() == Some(kind))
            .collect();
        let Some(first) = sites.iter().min_by_key(|r| (&r.path, r.line)) else {
            continue;
        };
        let at = format!("first at {}:{}", first.path, first.line);
        // Only the first untyped `new` is looked for: finding them all is
        // typing every `new` in the checkout.
        parts.push(match (kind, sites.len()) {
            (Unplaced::New, _) => format!("{one} ({at})"),
            (_, 1) => format!("1 {one} ({at})"),
            (_, n) => format!("{n} {many} ({at})"),
        });
    }
    if parts.is_empty() {
        return None;
    }
    let before = found.len();
    found.retain(|r| !kinds.iter().any(|(kind, ..)| r.unplaced() == Some(*kind)));
    counts.possible -= before - found.len();
    let parts = parts.join(", ");
    Some(match constructor {
        true => format!("constructed where its class is not known: {parts}"),
        false => parts,
    })
}
