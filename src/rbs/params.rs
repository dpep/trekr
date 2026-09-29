//! A stub's parameter list, from a method's rdoc call-seq and its RBS
//! overloads: named as the documentation names them, with the arity RBS
//! gives. Ported from the generator it replaces (`core_sigs.rb`, DEC-078).

use super::env::Method;
use super::parse::Function;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ParamKind {
    Req,
    Opt,
    Rest,
    Keyreq,
    Key,
    Keyrest,
    Block,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Param {
    pub(crate) kind: ParamKind,
    pub(crate) name: String,
    /// A default worth copying from a call-seq; anything else is `nil`.
    pub(crate) default: Option<String>,
}

impl Param {
    pub(crate) fn new(kind: ParamKind, name: impl Into<String>) -> Param {
        Param {
            kind,
            name: name.into(),
            default: None,
        }
    }

    fn positional(&self) -> bool {
        matches!(self.kind, ParamKind::Req | ParamKind::Opt)
    }
}

/// Ruby's keywords, which cannot name a positional parameter.
pub(crate) const KEYWORDS: [&str; 14] = [
    "true", "false", "nil", "self", "class", "module", "def", "end", "if", "unless", "do", "then",
    "in", "begin",
];

/// `initialize` and `new` are what every class overrides, so a variadic
/// signature is the honest one.
const VARIADIC: [&str; 2] = ["initialize", "new"];

pub(crate) fn is_ident(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `arg`, `arg0`: a name RBS gives for want of one.
fn synthetic(name: &str) -> bool {
    name.strip_prefix("arg")
        .is_some_and(|rest| rest.chars().all(|c| c.is_ascii_digit()))
}

/// A call-seq default worth copying: `nil`, `true`, a number, a short
/// string, a symbol, `$/`, `{}`, `[]`, a constant, `self.x`.
fn worth_copying(default: &str) -> bool {
    let quoted = |q: char| {
        default.len() >= 2
            && default.starts_with(q)
            && default.ends_with(q)
            && !default[1..default.len() - 1].contains(q)
    };
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    let number = |s: &str| {
        let s = s.strip_prefix('-').unwrap_or(s);
        let (whole, fraction) = s.split_once('.').unwrap_or((s, "0"));
        !whole.is_empty()
            && !fraction.is_empty()
            && whole.chars().all(|c| c.is_ascii_digit())
            && fraction.chars().all(|c| c.is_ascii_digit())
    };
    matches!(
        default,
        "nil" | "true" | "false" | "$/" | "$;" | "$," | "$>" | "{}" | "[]"
    ) || number(default)
        || quoted('\'')
        || quoted('"')
        || default.strip_prefix(':').is_some_and(word)
        || default.strip_prefix("self.").is_some_and(word)
        || (default.starts_with(|c: char| c.is_ascii_uppercase())
            && default
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':'))
}

/// A call-seq's argument text as parameters, or `None` when any part is
/// not one a `def` could say.
fn parse_params(text: &str) -> Option<Vec<Param>> {
    if text.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut parts = Vec::new();
    let mut depth = 0i32;
    let mut current = String::new();
    let text = text.replace("$,", "\0");
    for c in text.chars() {
        if "([{".contains(c) {
            depth += 1;
        }
        if ")]}".contains(c) {
            depth -= 1;
        }
        if c == ',' && depth == 0 {
            parts.push(current.trim().to_string());
            current.clear();
        } else {
            current.push(c);
        }
    }
    if !current.trim().is_empty() {
        parts.push(current.trim().to_string());
    }
    let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
    parts
        .into_iter()
        .map(|part| {
            let part = part.replace('\0', "$,");
            let mut param = if let Some(name) = part.strip_prefix('&').filter(|n| word(n)) {
                Param::new(ParamKind::Block, name)
            } else if let Some(name) = part.strip_prefix("**").filter(|n| word(n)) {
                Param::new(ParamKind::Keyrest, name)
            } else if let Some(name) = part.strip_prefix('*').filter(|n| word(n)) {
                Param::new(ParamKind::Rest, name)
            } else if let Some(name) = part.strip_suffix(':').filter(|n| word(n)) {
                Param::new(ParamKind::Keyreq, name)
            } else if let Some((name, default)) = part
                .split_once(':')
                .filter(|(n, d)| word(n) && !d.trim().is_empty())
            {
                Param {
                    default: Some(default.trim().to_string()),
                    ..Param::new(ParamKind::Key, name)
                }
            } else if let Some((name, default)) = part
                .split_once('=')
                .map(|(n, d)| (n.trim(), d.trim()))
                .filter(|(n, d)| word(n) && !d.is_empty())
            {
                Param {
                    default: Some(default.to_string()),
                    ..Param::new(ParamKind::Opt, name)
                }
            } else if word(&part) {
                Param::new(ParamKind::Req, part.as_str())
            } else {
                return None;
            };
            if !is_ident(&param.name) || KEYWORDS.contains(&param.name.as_str()) {
                return None;
            }
            if !param.default.as_deref().is_some_and(worth_copying) {
                param.default = None;
            }
            Some(param)
        })
        .collect()
}

pub(crate) fn render(params: &[Param]) -> String {
    params
        .iter()
        .map(|p| {
            let default = p.default.as_deref().unwrap_or("nil");
            match p.kind {
                ParamKind::Req => p.name.clone(),
                ParamKind::Opt => format!("{} = {default}", p.name),
                ParamKind::Rest => format!("*{}", p.name),
                ParamKind::Key => format!("{}: {default}", p.name),
                ParamKind::Keyreq => format!("{}:", p.name),
                ParamKind::Keyrest => format!("**{}", p.name),
                ParamKind::Block => format!("&{}", p.name),
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// The text inside the parentheses that open `text`, balanced.
fn parenthesized(text: &str) -> Option<&str> {
    if !text.starts_with('(') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in text.char_indices() {
        if c == '(' {
            depth += 1;
        }
        if c == ')' {
            depth -= 1;
        }
        if depth == 0 {
            return Some(&text[1..i]);
        }
    }
    None
}

/// Every form the rdoc call-seq lists for this method, as parameter lists.
/// The call-seq is the first `<!-- … -->` block of its comments.
fn call_seq_forms(method: &Method, name: &str) -> Vec<Vec<Param>> {
    let text = method.comments.join("\n");
    let Some(block) = text
        .split_once("<!--")
        .and_then(|(_, rest)| rest.split_once("-->"))
        .map(|(block, _)| block)
    else {
        return Vec::new();
    };
    block
        .lines()
        .filter_map(|line| {
            let form = line.trim_start().strip_prefix('-')?.trim_start();
            let form = form.split("->").next().unwrap_or(form).trim_end();
            let form = strip_receiver(form);
            let rest = form.strip_prefix(name)?;
            // A longer name that shares the prefix.
            if rest.starts_with(|c: char| c.is_ascii_alphanumeric() || "_?!=".contains(c)) {
                return None;
            }
            let args = if rest.starts_with('(') {
                parenthesized(rest)?
            } else {
                ""
            };
            let mut params = parse_params(args)?;
            let after = rest.strip_prefix(&format!("({args})")).unwrap_or(rest);
            if after.contains('{') && !params.iter().any(|p| p.kind == ParamKind::Block) {
                params.push(Param::new(ParamKind::Block, "block"));
            }
            Some(params)
        })
        .collect()
}

/// `File.join(…)` → `join(…)`: a receiver written before the name.
fn strip_receiver(form: &str) -> &str {
    let bytes = form.as_bytes();
    let mut i = 0;
    loop {
        let start = i;
        while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
            i += 1;
        }
        if i == start {
            return form;
        }
        if form[i..].starts_with("::") {
            i += 2;
            continue;
        }
        if bytes.get(i) == Some(&b'.') {
            return &form[i + 1..];
        }
        return form;
    }
}

/// The union of the forms: every slot any form names, required only where
/// every form requires it.
fn merge_forms(forms: &[Vec<Param>]) -> Vec<Param> {
    let positional: Vec<Vec<&Param>> = forms
        .iter()
        .map(|f| f.iter().filter(|p| p.positional()).collect())
        .collect();
    // The first of the widest, as Ruby's `max_by` picks.
    let widest = positional
        .iter()
        .fold(None::<&Vec<&Param>>, |best, f| match best {
            Some(b) if b.len() >= f.len() => Some(b),
            _ => Some(f),
        });
    let required = positional
        .iter()
        .map(|f| f.iter().filter(|p| p.kind == ParamKind::Req).count())
        .min()
        .unwrap_or(0);
    let mut merged: Vec<Param> = widest
        .map(|w| {
            w.iter()
                .enumerate()
                .map(|(i, p)| {
                    if i < required {
                        Param::new(ParamKind::Req, p.name.as_str())
                    } else {
                        Param {
                            default: p.default.clone(),
                            ..Param::new(ParamKind::Opt, p.name.as_str())
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let flat: Vec<&Param> = forms.iter().flatten().collect();
    if let Some(rest) = flat.iter().find(|p| p.kind == ParamKind::Rest) {
        merged.push((*rest).clone());
    }
    let mut keys: Vec<&str> = Vec::new();
    for p in flat.iter().filter(|p| p.kind == ParamKind::Key) {
        if !keys.contains(&p.name.as_str()) {
            keys.push(&p.name);
            merged.push((*p).clone());
        }
    }
    if let Some(keyrest) = flat.iter().find(|p| p.kind == ParamKind::Keyrest) {
        merged.push((*keyrest).clone());
    }
    if let Some(block) = flat.iter().find(|p| p.kind == ParamKind::Block) {
        merged.push((*block).clone());
    }
    merged
}

fn functions(method: &Method) -> Vec<&Function> {
    method
        .overloads
        .iter()
        .filter_map(|o| o.function.as_ref())
        .collect()
}

fn slots(f: &Function) -> Vec<&Option<String>> {
    f.required.iter().chain(&f.optional).collect()
}

/// Parameters from RBS alone, when the call-seq has none worth reading.
fn rbs_params(method: &Method, existing: &[Param]) -> Vec<Param> {
    let fns = functions(method);
    if fns.is_empty() {
        return existing.to_vec();
    }
    // The widest overload names its slots most consistently.
    let mut all: Vec<Vec<&Option<String>>> = fns.iter().map(|f| slots(f)).collect();
    all.sort_by_key(|slot| std::cmp::Reverse(slot.len()));
    let names: Vec<Option<String>> = (0..all[0].len())
        .map(|i| {
            all.iter()
                .filter_map(|slot| slot.get(i).and_then(|n| n.as_ref()))
                .find(|n| !synthetic(n))
                .cloned()
        })
        .collect();
    let mut distinct = names.clone();
    distinct.sort();
    distinct.dedup();
    // A name RBS does not give is better left as the stub wrote it than
    // invented.
    if names.iter().any(Option::is_none) || distinct.len() < names.len() {
        return existing.to_vec();
    }
    let mut params: Vec<Param> = names
        .into_iter()
        .flatten()
        .map(|name| Param::new(ParamKind::Opt, name))
        .collect();
    let mut keys: Vec<&String> = Vec::new();
    for f in &fns {
        for key in f.required_keywords.iter().chain(&f.optional_keywords) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    for key in keys {
        let required = fns.iter().all(|f| f.required_keywords.contains(key));
        let kind = if required {
            ParamKind::Keyreq
        } else {
            ParamKind::Key
        };
        params.push(Param::new(kind, key.as_str()));
    }
    if let Some(keyrest) = fns.iter().find_map(|f| f.rest_keywords.as_ref()) {
        params.push(Param::new(
            ParamKind::Keyrest,
            keyrest.as_deref().unwrap_or("options"),
        ));
    }
    params
}

/// Make a parameter list agree with what RBS says Ruby accepts: required
/// only as far as every overload requires, variadic when any overload is,
/// a block when any takes one, and every name distinct.
fn reconcile(mut params: Vec<Param>, method: &Method, existing: &[Param]) -> Vec<Param> {
    let fns = functions(method);
    if !fns.is_empty() {
        let lead = fns.iter().map(|f| f.required.len()).min().unwrap_or(0);
        for (i, p) in params.iter_mut().filter(|p| p.positional()).enumerate() {
            p.kind = if i < lead {
                ParamKind::Req
            } else {
                ParamKind::Opt
            };
        }
        // A form the call-seq could not spell still takes its arguments.
        let widest = fns
            .iter()
            .map(|f| slots(f))
            .fold(None::<Vec<&Option<String>>>, |best, s| match best {
                Some(b) if b.len() >= s.len() => Some(b),
                _ => Some(s),
            })
            .unwrap_or_default();
        let taken = params.iter().filter(|p| p.positional()).count();
        if taken < widest.len() && !params.iter().any(|p| p.kind == ParamKind::Rest) {
            let at = params
                .iter()
                .position(|p| !p.positional())
                .unwrap_or(params.len());
            let extra: Vec<Param> = widest[taken..]
                .iter()
                .enumerate()
                .map(|(i, slot)| {
                    let name = (*slot)
                        .clone()
                        .unwrap_or_else(|| format!("arg{}", taken + i + 1));
                    Param::new(ParamKind::Opt, name)
                })
                .collect();
            params.splice(at..at, extra);
        }
        if fns.iter().any(|f| f.rest.is_some()) && !params.iter().any(|p| p.kind == ParamKind::Rest)
        {
            let name = existing
                .iter()
                .find(|p| p.kind == ParamKind::Rest)
                .map(|p| p.name.clone())
                .or_else(|| {
                    fns.iter()
                        .filter_map(|f| f.rest.as_ref().and_then(|r| r.clone()))
                        .find(|n| !synthetic(n))
                })
                .unwrap_or_else(|| "args".to_string());
            let at = params
                .iter()
                .position(|p| {
                    matches!(
                        p.kind,
                        ParamKind::Key | ParamKind::Keyreq | ParamKind::Keyrest | ParamKind::Block
                    )
                })
                .unwrap_or(params.len());
            params.insert(at, Param::new(ParamKind::Rest, name));
        }
    }
    if method.overloads.iter().any(|o| o.block.is_some())
        && !params.iter().any(|p| p.kind == ParamKind::Block)
    {
        params.push(Param::new(ParamKind::Block, "block"));
    }
    let positional: Vec<String> = params
        .iter()
        .filter(|p| matches!(p.kind, ParamKind::Req | ParamKind::Opt | ParamKind::Rest))
        .map(|p| p.name.clone())
        .collect();
    params.retain(|p| {
        !(matches!(p.kind, ParamKind::Key | ParamKind::Keyreq) && positional.contains(&p.name))
    });
    // A keyword may name a keyword argument (`in:`), never a positional one.
    for p in &mut params {
        if matches!(p.kind, ParamKind::Key | ParamKind::Keyreq)
            || !KEYWORDS.contains(&p.name.as_str())
        {
            continue;
        }
        p.name = match p.name.as_str() {
            "class" => "klass".to_string(),
            "module" => "mod".to_string(),
            other => format!("{other}_"),
        };
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for p in &mut params {
        let count = seen.entry(p.name.clone()).or_default();
        *count += 1;
        if *count > 1 {
            p.name = if p.kind == ParamKind::Rest {
                "args".to_string()
            } else {
                format!("{}{count}", p.name)
            };
        }
    }
    params
}

/// A core or compiled method's parameters: its call-seq's names, RBS's
/// arity. An operator reads better with RBS's own (`+(other)`) than a
/// call-seq (`string + other_string`) can say.
pub(crate) fn for_method(method: &Method) -> Vec<Param> {
    let name = method.name.as_str();
    if VARIADIC.contains(&name) {
        return variadic(method);
    }
    let existing = unnamed(method);
    let plain = name
        .strip_suffix(['?', '!', '='])
        .unwrap_or(name)
        .chars()
        .enumerate()
        .all(|(i, c)| c.is_ascii_alphanumeric() && (i > 0 || !c.is_ascii_digit()) || c == '_');
    if !plain {
        return untyped(reconcile(existing.clone(), method, &existing), method);
    }
    let forms = call_seq_forms(method, name);
    let params = if forms.is_empty() {
        rbs_params(method, &existing)
    } else {
        merge_forms(&forms)
    };
    untyped(reconcile(params, method, &existing), method)
}

/// `(?)` says nothing of the parameters, so any count is taken:
/// `Proc#call`'s.
fn untyped(mut params: Vec<Param>, method: &Method) -> Vec<Param> {
    let open = method.overloads.iter().any(|o| o.function.is_none());
    if open && !params.iter().any(|p| p.kind == ParamKind::Rest) {
        let at = params
            .iter()
            .position(|p| !p.positional())
            .unwrap_or(params.len());
        params.insert(at, Param::new(ParamKind::Rest, "args"));
    }
    params
}

/// `initialize` is none when RBS says it takes none, else any; `new` is
/// always any, since RBS's `Class#new` is written as taking none and each
/// class's is its `initialize`'s.
fn variadic(method: &Method) -> Vec<Param> {
    let takes_none = method.name == "initialize"
        && !method.overloads.is_empty()
        && method.overloads.iter().all(|o| {
            o.function
                .as_ref()
                .is_some_and(|f| *f == Function::default())
                && o.block.is_none()
        });
    if takes_none {
        return Vec::new();
    }
    let mut params = vec![Param::new(ParamKind::Rest, "args")];
    if method.overloads.iter().any(|o| o.block.is_some()) {
        params.push(Param::new(ParamKind::Block, "block"));
    }
    params
}

/// The parameters a stub writes when it knows no names: RBS's own, or
/// `other` for an operator's one argument, else by position.
fn unnamed(method: &Method) -> Vec<Param> {
    let fns = functions(method);
    let Some(widest) =
        fns.iter()
            .map(|f| slots(f))
            .fold(None::<Vec<&Option<String>>>, |best, s| match best {
                Some(b) if b.len() >= s.len() => Some(b),
                _ => Some(s),
            })
    else {
        return Vec::new();
    };
    let single = widest.len() == 1;
    widest
        .iter()
        .enumerate()
        .map(|(i, slot)| {
            let name = slot
                .as_ref()
                .filter(|n| !synthetic(n) && is_ident(n) && !KEYWORDS.contains(&n.as_str()))
                .cloned()
                .unwrap_or_else(|| {
                    if single {
                        "other".to_string()
                    } else {
                        format!("arg{}", i + 1)
                    }
                });
            Param::new(ParamKind::Opt, name)
        })
        .collect()
}

/// A keyword RBS names that a parameter cannot (`scrypt`'s `N:`) is taken by
/// a `**options` instead.
pub(crate) fn spellable(params: Vec<Param>) -> Vec<Param> {
    let (bad, mut good): (Vec<Param>, Vec<Param>) = params
        .into_iter()
        .partition(|p| matches!(p.kind, ParamKind::Key | ParamKind::Keyreq) && !is_ident(&p.name));
    if bad.is_empty() || good.iter().any(|p| p.kind == ParamKind::Keyrest) {
        return good;
    }
    let at = good
        .iter()
        .position(|p| p.kind == ParamKind::Block)
        .unwrap_or(good.len());
    good.insert(at, Param::new(ParamKind::Keyrest, "options"));
    good
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rbs::env::{Env, Source};
    use crate::rbs::parse;

    fn method(rbs: &str, name: &str) -> Method {
        let env = Env::build(&[Source {
            library: None,
            parsed: parse::parse(rbs),
        }]);
        env.classes["W"]
            .methods
            .values()
            .find(|m| m.name == name)
            .expect("the method")
            .clone()
    }

    fn params(rbs: &str, name: &str) -> String {
        render(&for_method(&method(rbs, name)))
    }

    #[test]
    fn names_come_from_the_call_seq_and_arity_from_rbs() {
        let rbs = "class W\n  # <!--\n  #   rdoc-file=string.c\n  #   - downcase(*options) -> string\n  # -->\n  def downcase: (*untyped) -> String\nend\n";
        assert_eq!(params(rbs, "downcase"), "*options");
        let rbs = "class W\n  # <!--\n  #   rdoc-file=x.c\n  #   - str.index(substring, offset = 0) -> integer or nil\n  # -->\n  def index: (String, ?int) -> Integer?\nend\n";
        assert_eq!(params(rbs, "index"), "substring, offset = 0");
    }

    #[test]
    fn a_form_the_call_seq_misses_is_still_taken_and_a_block_is_added() {
        let rbs = "class W\n  # <!--\n  #   - each_slice(n) { ... } -> nil\n  # -->\n  def each_slice: (Integer n, ?Integer m) { (untyped) -> void } -> self\nend\n";
        assert_eq!(params(rbs, "each_slice"), "n, m = nil, &block");
    }

    #[test]
    fn without_a_call_seq_rbs_names_them_or_they_are_numbered() {
        assert_eq!(
            params(
                "class W\n  def go: (Integer count, ?String sep) -> nil\nend\n",
                "go"
            ),
            "count, sep = nil"
        );
        assert_eq!(
            params("class W\n  def go: (Integer, String) -> nil\nend\n", "go"),
            "arg1, arg2"
        );
        assert_eq!(
            params("class W\n  def +: (Integer) -> Integer\nend\n", "+"),
            "other"
        );
        assert_eq!(
            params(
                "class W\n  def []=: (Integer, untyped) -> untyped\nend\n",
                "[]="
            ),
            "arg1, arg2"
        );
    }

    #[test]
    fn untyped_parameters_take_any_count() {
        assert_eq!(
            params("class W\n  def call: (?) -> untyped\nend\n", "call"),
            "*args"
        );
        assert_eq!(
            params("class W\n  def ===: (?) -> untyped\nend\n", "==="),
            "*args"
        );
    }

    #[test]
    fn initialize_is_variadic_unless_it_takes_nothing_and_new_always_is() {
        assert_eq!(
            params("class W\n  def initialize: () -> void\nend\n", "initialize"),
            ""
        );
        assert_eq!(
            params("class W\n  def new: () -> untyped\nend\n", "new"),
            "*args"
        );
        assert_eq!(
            params(
                "class W\n  def initialize: (Integer) { () -> void } -> void\nend\n",
                "initialize"
            ),
            "*args, &block"
        );
    }

    #[test]
    fn a_keyword_named_parameter_is_renamed() {
        assert_eq!(
            params("class W\n  def go: (Class class) -> nil\nend\n", "go"),
            "klass"
        );
    }

    #[test]
    fn reads_call_seq_parameters_it_can_spell_and_refuses_the_rest() {
        let parsed = parse_params("a, b = 1, *rest, key: :x, &blk").unwrap();
        assert_eq!(render(&parsed), "a, b = 1, *rest, key: :x, &blk");
        assert!(parse_params("a [, b]").is_none());
        assert_eq!(render(&parse_params("sep = $,").unwrap()), "sep = $,");
        assert_eq!(render(&parse_params("x = compute()").unwrap()), "x = nil");
    }
}
