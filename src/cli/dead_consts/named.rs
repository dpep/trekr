//! The ways a checkout names a class without a constant reference, read from
//! its Ruby and templates as text (its YAML is `config`'s, DEC-402), as the views (DEC-315) and built names
//! (DEC-363) are: what `--dead` weighs a class nothing references against
//! (DEC-421).
//!
//! Not a parse. Each find is evidence that a name is looked up at runtime,
//! never a resolved reference.

use super::super::built::{TEMPLATES, Texts};
use rayon::prelude::*;
use std::collections::HashMap;

/// Where the checkout writes something: the file, relative to the checkout,
/// and the line.
pub(super) type At = (String, u32);

#[derive(Default)]
pub(super) struct Named {
    /// A whole string literal spelled as a constant path (`"Admin::Widget"`),
    /// and a YAML value that is one (`class: Jobs::Purge`), as written.
    pub(super) strings: HashMap<String, At>,
    /// The shape of an interpolated string handed to `constantize` or
    /// `const_get` (`Jobs::*` for `"Jobs::#{type.camelize}"`).
    pub(super) shapes: Vec<(String, At)>,
    /// Of those, the ones built from a class's name
    /// (`"#{self.class.name}Drop"`), by shape.
    pub(super) class_shapes: Vec<(String, At)>,
    /// Each symbol and hash key, `plain` (`:date_of_birth` → `dateofbirth`).
    pub(super) symbols: HashMap<String, Vec<At>>,
    /// The class an association names by convention, `plain`
    /// (`has_many :line_items` → `lineitem`).
    pub(super) associated: HashMap<String, (String, At)>,
    /// A routes file's path-like string (`'auth/passwords'`), with
    /// `_controller` added: a gem's routes (`devise_for … controllers:`)
    /// name a controller so.
    pub(super) route_strings: HashMap<String, At>,
    /// A constant whose subclasses are listed at runtime.
    pub(super) listed: HashMap<String, At>,
    /// A constant whose own constants are listed at runtime (`X.constants`),
    /// or looked up by a name computed at runtime (`X.const_get(region)`).
    pub(super) constants_listed: HashMap<String, (String, At)>,
    /// A constant read on a value, by its last segment: `self.class::LIMIT`,
    /// `const_get(:LIMIT)` — no reference trekr can resolve.
    pub(super) dynamic: HashMap<String, At>,
    /// A routes file's call of a gem's routes, which are not read
    /// (`devise_for`, `use_doorkeeper`, `mount`).
    pub(super) gem_routes: Vec<(String, At)>,
    /// Each file that lists the constants of a namespace it computes from a
    /// class's name (`self.class.name.deconstantize.constantize.constants`),
    /// with the line.
    pub(super) lists_namespace: HashMap<String, u32>,
    /// The first line of this file that looks a constant up by a name it
    /// computes (`constantize`, `const_get`): per file, merged into
    /// `computed_in`.
    computed: Option<u32>,
    /// Each file with such a line, and the first.
    pub(super) computed_in: HashMap<String, u32>,
    /// The first lookup of a name computed whole, which no namespace or
    /// shape bounds (`name.constantize`, `Object.const_get(x)`,
    /// `klass.const_get(x)`): the call, and where. Any constant may be its
    /// answer (DEC-654).
    pub(super) unscoped: Option<(String, At)>,
    /// The constant paths an executable Ruby script with no `.rb` writes
    /// (`bin/cli`), which the index does not read: each with the script.
    pub(super) script_constants: HashMap<String, String>,
}

/// The calls in a routes file that draw a gem's routes.
const GEM_ROUTES: [&str; 3] = ["devise_for", "use_doorkeeper", "mount"];

/// Lowercase, without underscores or `::`: how an inflected name and the
/// constant it camelizes to are compared, acronyms included.
pub(super) fn plain(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_' && *c != ':')
        .flat_map(char::to_lowercase)
        .collect()
}

impl Named {
    /// The checkout's Ruby, rake, gemspec and template files git knows, and
    /// its scripts with no extension, less its tests and `db/`, as read once
    /// for `--dead`.
    pub(super) fn read(texts: &Texts) -> Named {
        let parts: Vec<Named> = texts
            .files
            .par_iter()
            .filter_map(|(path, text)| {
                let mut part = Named::default();
                let script = !path
                    .rsplit('/')
                    .next()
                    .is_some_and(|name| name.contains('.'));
                if script {
                    if text.starts_with("#!")
                        && text
                            .lines()
                            .next()
                            .is_some_and(|first| first.contains("ruby"))
                    {
                        for constant in text.lines().flat_map(super::super::views::constant_paths) {
                            part.script_constants
                                .entry(constant)
                                .or_insert_with(|| path.clone());
                        }
                    }
                } else if TEMPLATES.iter().any(|ext| path.ends_with(ext)) {
                    for (n, line) in text.lines().enumerate() {
                        part.read_dynamic(path, n as u32 + 1, line);
                    }
                } else {
                    part.read_ruby(path, text);
                    if let Some(line) = part.computed.take() {
                        part.computed_in.insert(path.clone(), line);
                    }
                }
                Some(part)
            })
            .collect();
        let mut named = Named::default();
        for part in parts {
            named.absorb(part);
        }
        named
    }

    /// Another file's finds, keeping each where it was first written.
    fn absorb(&mut self, part: Named) {
        for (key, at) in part.strings {
            self.strings.entry(key).or_insert(at);
        }
        for (shape, at) in part.shapes {
            if !self.shapes.iter().any(|(known, _)| *known == shape) {
                self.shapes.push((shape, at));
            }
        }
        for (shape, at) in part.class_shapes {
            if !self.class_shapes.iter().any(|(known, _)| *known == shape) {
                self.class_shapes.push((shape, at));
            }
        }
        for (key, ats) in part.symbols {
            for at in ats {
                self.symbol_at(key.clone(), at);
            }
        }
        for (key, at) in part.associated {
            self.associated.entry(key).or_insert(at);
        }
        for (key, at) in part.route_strings {
            self.route_strings.entry(key).or_insert(at);
        }
        for (key, at) in part.listed {
            self.listed.entry(key).or_insert(at);
        }
        for (key, at) in part.constants_listed {
            self.constants_listed.entry(key).or_insert(at);
        }
        for (key, at) in part.dynamic {
            self.dynamic.entry(key).or_insert(at);
        }
        self.gem_routes.extend(part.gem_routes);
        for (key, at) in part.lists_namespace {
            self.lists_namespace.entry(key).or_insert(at);
        }
        for (key, at) in part.computed_in {
            self.computed_in.entry(key).or_insert(at);
        }
        if self.unscoped.is_none() {
            self.unscoped = part.unscoped;
        }
        for (key, at) in part.script_constants {
            self.script_constants.entry(key).or_insert(at);
        }
    }

    /// Where a symbol is written, in at most two files: enough to find one
    /// outside the file that defines what it names.
    fn symbol_at(&mut self, key: String, at: At) {
        let ats = self.symbols.entry(key).or_default();
        if ats.len() < 2 && !ats.iter().any(|(path, _)| *path == at.0) {
            ats.push(at);
        }
    }

    /// Where a symbol is written outside `path`.
    pub(super) fn symbol_outside(&self, key: &str, path: &str) -> Option<&At> {
        self.symbols.get(key)?.iter().find(|(file, _)| file != path)
    }

    /// The constants a file outside the checkout — a gem's base class —
    /// reads on a value, by last segment.
    pub(super) fn read_on_a_value(path: &str) -> HashMap<String, At> {
        let mut named = Named::default();
        if let Ok(bytes) = crate::scan::read_source(path) {
            for (n, line) in String::from_utf8_lossy(&bytes).lines().enumerate() {
                named.read_dynamic(path, n as u32 + 1, line);
            }
        }
        named.dynamic
    }

    /// `value::NAME`, and `const_get(:NAME)`: a constant read where the
    /// namespace is a value.
    fn read_dynamic(&mut self, path: &str, line_no: u32, line: &str) {
        let bytes = line.as_bytes();
        let mut rest = 0;
        while let Some(found) = line[rest..].find("::") {
            let at = rest + found;
            rest = at + 2;
            let before = at.checked_sub(1).map(|i| bytes[i]);
            let value = before.is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_)]".contains(&b))
                // `Foo::Bar` is a path; `foo::Bar` and `foo.class::Bar` are not.
                && !line[..at]
                    .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .next()
                    .is_some_and(|word| word.starts_with(|c: char| c.is_ascii_uppercase()));
            if value && bytes.get(at + 2).is_some_and(u8::is_ascii_uppercase) {
                let name = word_at(line, at + 2);
                self.dynamic
                    .entry(name.to_string())
                    .or_insert_with(|| (path.to_string(), line_no));
            }
        }
        if let Some(open) = line.find("const_get(") {
            let arg = line[open + 10..].trim_start_matches([':', '"', '\'']);
            if arg.starts_with(|c: char| c.is_ascii_uppercase()) {
                self.dynamic
                    .entry(word_at(arg, 0).to_string())
                    .or_insert_with(|| (path.to_string(), line_no));
            }
        }
    }

    fn read_ruby(&mut self, path: &str, text: &str) {
        let routes = path.starts_with("config/routes") || path.ends_with("/config/routes.rb");
        // `steps = Steps`, so that `steps.constants` lists `Steps`'.
        let mut assigned: HashMap<&str, &str> = HashMap::new();
        let mut previous = "";
        if text.contains("deconstantize") && text.contains(".constants") {
            let line = text
                .lines()
                .position(|l| l.contains("deconstantize"))
                .unwrap_or(0);
            self.lists_namespace
                .insert(path.to_string(), line as u32 + 1);
        }
        // A word each file writes many times is looked up once.
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        let mut block_comment = false;
        // The heredocs opened and not yet closed: each one's terminator, and
        // whether its body is code (handed to an eval) rather than text.
        let mut heredocs: Vec<(&str, bool)> = Vec::new();
        for (n, line) in text.lines().enumerate() {
            if block_comment {
                block_comment = !line.starts_with("=end");
                continue;
            }
            if line.starts_with("=begin") {
                block_comment = true;
                continue;
            }
            let text_body = match heredocs.first() {
                Some((end, _)) if line.trim() == *end => {
                    heredocs.remove(0);
                    continue;
                }
                Some((_, code)) => !code,
                None => false,
            };
            if heredocs.is_empty() {
                heredocs.extend(heredoc_openers(line));
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with('#') {
                continue;
            }
            let line_no = n as u32 + 1;
            let at = || (path.to_string(), line_no);
            self.read_dynamic(path, line_no, line);
            // `inflect.acronym 'API'` spells an inflection, not a constant.
            if line.contains("acronym") {
                continue;
            }
            if routes
                && let Some(call) = GEM_ROUTES.iter().find(|call| {
                    trimmed
                        .strip_prefix(**call)
                        .is_some_and(|r| r.starts_with([' ', '(']))
                })
            {
                self.gem_routes.push((call.to_string(), at()));
            }
            let built = !text_body && (line.contains("constantize") || line.contains("const_get"));
            let bytes = line.as_bytes();
            let mut i = 0;
            while i < bytes.len() {
                let c = bytes[i];
                if c == b'#' && !(i + 1 < bytes.len() && bytes[i + 1] == b'{') {
                    break; // a trailing comment
                }
                if c == b'"' || c == b'\'' {
                    let mut end = i + 1;
                    while end < bytes.len() && bytes[end] != c {
                        end += if bytes[end] == b'\\' { 2 } else { 1 };
                    }
                    if end >= bytes.len() {
                        break;
                    }
                    let content = &line[i + 1..end];
                    i = end + 1;
                    if c == b'"' && content.contains("#{") {
                        if built && let Some(shape) = shape_of(content) {
                            if content.contains(".name}") || content.contains(".class}") {
                                self.class_shapes.push((shape.clone(), at()));
                            }
                            self.shapes.push((shape, at()));
                        }
                    } else if is_constant_path(content)
                        && (content.contains("::") || names_a_class(line))
                        && !inflects(line, content)
                    {
                        if !self.strings.contains_key(content) {
                            self.strings.insert(content.to_string(), at());
                        }
                    } else if routes && is_route_path(content) {
                        self.route_strings
                            .entry(format!("{content}_controller"))
                            .or_insert_with(at);
                    }
                    continue;
                }
                let word_start =
                    i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
                // `:name`, not `::Name` nor `a ? b : c`.
                if c == b':'
                    && bytes
                        .get(i + 1)
                        .is_some_and(|b| b.is_ascii_lowercase() || *b == b'_')
                    && (i == 0
                        || bytes[i - 1] != b':'
                            && !bytes[i - 1].is_ascii_alphanumeric()
                            && bytes[i - 1] != b'_')
                {
                    let word = word_at(line, i + 1);
                    if seen.insert(word) {
                        self.symbol_at(plain(word), at());
                    }
                    i += 1 + word.len();
                    continue;
                }
                // `name:`, a hash key or keyword, not `name::`.
                if c.is_ascii_lowercase() && word_start {
                    let word = word_at(line, i);
                    let after = i + word.len();
                    if bytes.get(after) == Some(&b':')
                        && bytes.get(after + 1) != Some(&b':')
                        && seen.insert(word)
                    {
                        self.symbol_at(plain(word), at());
                    }
                    i = after.max(i + 1);
                    continue;
                }
                i += 1;
            }
            match association(trimmed) {
                Some(Named_::Class(class)) => {
                    self.associated
                        .entry(plain(&class))
                        .or_insert_with(|| (class, at()));
                }
                Some(Named_::Shape(shape)) => self.shapes.push((shape, at())),
                None => {}
            }
            if built && self.computed.is_none() {
                self.computed = Some(line_no);
            }
            if built
                && self.unscoped.is_none()
                && let Some(call) = unscoped_lookup(line)
            {
                self.unscoped = Some((call, at()));
            }
            for listed in listings(line, &[".descendants", ".subclasses"]) {
                self.listed.entry(listed).or_insert_with(at);
            }
            match object_space_listing(line).filter(|_| !text_body) {
                Some(Some(base)) => {
                    self.listed.entry(base).or_insert_with(at);
                }
                Some(None) if self.unscoped.is_none() => {
                    self.unscoped = Some(("ObjectSpace.each_object".to_string(), at()));
                }
                _ => {}
            }
            if let Some((name, value)) = trimmed.split_once(" = ")
                && name
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                && is_constant_path(value.trim_end())
            {
                assigned.insert(name, value.trim_end());
            }
            // `steps.constants`, or `steps` with `.constants` on the next line.
            let chained = trimmed.strip_prefix(".constants").is_some_and(|rest| {
                !rest.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
            });
            let held = assigned.iter().filter_map(|(name, constant)| {
                let call = format!("{name}.constants");
                let written = line.match_indices(&call).any(|(at, _)| {
                    !line[..at]
                        .ends_with(|c: char| c.is_ascii_alphanumeric() || c == '_' || c == '.')
                        && !line[at + call.len()..]
                            .starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_')
                });
                (written || (chained && previous == *name))
                    .then(|| constant.trim_start_matches("::").to_string())
            });
            for listed in listings(line, &[".constants"])
                .into_iter()
                .chain(held.collect::<Vec<_>>())
            {
                self.constants_listed
                    .entry(listed)
                    .or_insert_with(|| ("constants".to_string(), at()));
            }
            if !trimmed.is_empty() {
                previous = trimmed.trim_end();
            }
            for namespace in computed_const_gets(line).into_iter().filter(|_| built) {
                self.constants_listed
                    .entry(namespace)
                    .or_insert_with(|| ("const_get".to_string(), at()));
            }
        }
    }
}

/// Does a line hand a string to something that looks a class up by it? A
/// string of one capitalised word is a name only there: elsewhere it is a
/// word (`type: 'Application'`).
fn names_a_class(line: &str) -> bool {
    [
        "class_name",
        "constantize",
        "const_get",
        "class:",
        "klass",
        "_class",
        "Class",
    ]
    .iter()
    .any(|context| line.contains(context))
}

/// `"clean_up_ips" => "CleanUpIPs"`: an inflection mapping a file's name to
/// the constant it defines, which names no use of it.
fn inflects(line: &str, constant: &str) -> bool {
    line.contains("=>")
        && line
            .split(['"', '\''])
            .any(|part| part != constant && !part.is_empty() && plain(part) == plain(constant))
}

/// The identifier starting at `at`.
fn word_at(line: &str, at: usize) -> &str {
    let rest = &line[at..];
    let end = rest
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(rest.len());
    &rest[..end]
}

/// `Widget`, `::Admin::Widget`: a capitalised name, and segments of one.
fn is_constant_path(text: &str) -> bool {
    let text = text.strip_prefix("::").unwrap_or(text);
    !text.is_empty()
        && text.split("::").all(|segment| {
            segment.starts_with(|c: char| c.is_ascii_uppercase())
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

/// `auth/passwords`: a controller's path as a route writes it.
fn is_route_path(text: &str) -> bool {
    text.contains('/')
        && !text.starts_with('/')
        && text
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '/')
}

/// The shape a constantized string spells, `*` for each interpolation: none
/// unless what is written names three characters of a constant, or a whole
/// namespace (`UI::*`), which bounds it however short.
fn shape_of(content: &str) -> Option<String> {
    let mut shape = String::new();
    let mut rest = content;
    while let Some(open) = rest.find("#{") {
        shape.push_str(&rest[..open]);
        shape.push('*');
        let inner = &rest[open + 2..];
        rest = inner.find('}').map_or("", |close| &inner[close + 1..]);
    }
    shape.push_str(rest);
    let shape = shape.strip_prefix("::").unwrap_or(&shape).to_string();
    let written = shape.chars().filter(|c| c.is_ascii_alphanumeric()).count();
    let namespace = shape
        .split_once("::")
        .is_some_and(|(head, _)| is_constant_path(head));
    ((written >= 3 || namespace)
        && shape
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == ':' || c == '*'))
    .then_some(shape)
}

/// The class `has_many :line_items` names by convention, unless it says
/// another (`class_name:`, a string) or none (`polymorphic: true`).
///
/// A name built at runtime (`has_one :"#{name.underscore}_search_data"`)
/// names the shape of a class instead: `*SearchData`.
fn association(line: &str) -> Option<Named_> {
    let (plural, rest) = [
        ("has_and_belongs_to_many", true),
        ("has_many", true),
        ("has_one", false),
        ("belongs_to", false),
    ]
    .iter()
    .find_map(|(call, plural)| {
        let at = line.find(call)?;
        let before = line[..at].chars().next_back();
        let rest = line[at + call.len()..].strip_prefix([' ', '('])?;
        (!before.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.'))
            .then_some((*plural, rest))
    })?;
    if line.contains("class_name") || line.contains("polymorphic") {
        return None;
    }
    let symbol = rest.trim_start().strip_prefix(':')?;
    let class = |name: &str| match plural {
        true => crate::extract::table_to_class(name),
        false => crate::extract::camelize(name),
    };
    let Some(quoted) = symbol.strip_prefix('"') else {
        return Some(Named_::Class(class(word_at(symbol, 0))));
    };
    let quoted = &quoted[..quoted.find('"')?];
    let shape = shape_of(quoted)?;
    let (head, tail) = shape.rsplit_once('*')?;
    let head = if head.is_empty() {
        String::new()
    } else {
        crate::extract::camelize(head)
    };
    Some(Named_::Shape(format!("{head}*{}", class(tail))))
}

/// What an association names: a class, or the shape of one built at runtime.
enum Named_ {
    Class(String),
    Shape(String),
}

/// `Regions.const_get(name)`, `Module.const_get("Regions").const_get(name)`:
/// the namespaces a line looks a constant up in by a name it computes.
fn computed_const_gets(line: &str) -> Vec<String> {
    let mut found = Vec::new();
    let call = ".const_get(";
    let mut rest = line;
    while let Some(at) = rest.find(call) {
        let receiver = &rest[..at];
        let arg = rest[at + call.len()..].trim_start();
        rest = &rest[at + call.len()..];
        let literal = arg
            .strip_prefix(':')
            .or_else(|| {
                arg.strip_prefix('"')
                    .filter(|a| !a.split('"').next().unwrap_or("").contains("#{"))
            })
            .or_else(|| arg.strip_prefix('\''))
            .is_some_and(|a| a.starts_with(|c: char| c.is_ascii_uppercase()));
        if literal {
            continue;
        }
        let namespace = match receiver.strip_suffix("\")") {
            Some(inner) => inner.rsplit('"').next().unwrap_or(""),
            None => {
                let start = receiver
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                    .map_or(0, |i| i + 1);
                &receiver[start..]
            }
        };
        if is_constant_path(namespace)
            && namespace != "Object"
            && namespace != "Module"
            && namespace != "Kernel"
        {
            found.push(namespace.trim_start_matches("::").to_string());
        }
    }
    found
}

/// The heredocs a line opens (`<<~SQL`, `<<-'RUBY'`), each with its
/// terminator and whether its body is code: one handed to an eval
/// (`class_eval <<~RUBY`) is, any other is a string's text.
fn heredoc_openers(line: &str) -> Vec<(&str, bool)> {
    let (spans, code_end) = string_spans(line);
    let code = line.contains("eval");
    let mut found = Vec::new();
    for (at, _) in line[..code_end].match_indices("<<") {
        if spans.iter().any(|(o, c)| *o < at && at < *c) {
            continue;
        }
        let rest = &line[at + 2..];
        let (squiggly, rest) = match rest.strip_prefix(['~', '-']) {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let quoted = rest.starts_with(['\'', '"']);
        let name = word_at(rest.trim_start_matches(['\'', '"']), 0);
        // `list <<item` is a push; a bare heredoc name is capitalised.
        if !name.is_empty()
            && !name.starts_with(|c: char| c.is_ascii_digit())
            && (squiggly || quoted || name.starts_with(|c: char| c.is_ascii_uppercase()))
        {
            found.push((name, code));
        }
    }
    found
}

/// The quoted strings of a line, `(open, close)` quote offsets, and where
/// its code ends: at a trailing comment, or the line's end.
fn string_spans(line: &str) -> (Vec<(usize, usize)>, usize) {
    let bytes = line.as_bytes();
    let mut spans = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'#' && bytes.get(i + 1) != Some(&b'{') {
            return (spans, i);
        }
        if c == b'"' || c == b'\'' {
            let mut end = i + 1;
            while end < bytes.len() && bytes[end] != c {
                end += if bytes[end] == b'\\' { 2 } else { 1 };
            }
            if end >= bytes.len() {
                return (spans, i);
            }
            spans.push((i, end));
            i = end + 1;
            continue;
        }
        i += 1;
    }
    (spans, bytes.len())
}

/// A lookup on `line` of a name computed whole, which neither a namespace
/// nor a written part of the name bounds: the call as it is cited
/// (`safe_constantize`, `Object.const_get`, `klass.const_get`).
///
/// `"Jobs::#{type}".constantize` has a shape, `Regions.const_get(name)` a
/// namespace, and `constants.map { |c| const_get(c) }` lists one: each is
/// read for what it bounds, not here.
fn unscoped_lookup(line: &str) -> Option<String> {
    let (spans, code_end) = string_spans(line);
    let bytes = line.as_bytes();
    let in_code = |at: usize| at < code_end && !spans.iter().any(|(o, c)| *o < at && at < *c);
    // Is the value starting (or ending, for a receiver) at a quote one whose
    // whole name is computed? A literal is a string rule's; a shape bounds it.
    let computed = |quote: usize, closing: bool| {
        let Some((open, close)) = spans
            .iter()
            .find(|(o, c)| if closing { *c == quote } else { *o == quote })
        else {
            return true;
        };
        let content = &line[open + 1..*close];
        bytes[*open] == b'"' && content.contains("#{") && shape_of(content).is_none()
    };
    let computed_arg = |from: usize| {
        let rest = &line[from..];
        let arg = rest.trim_start_matches(['(', ' ']);
        let at = from + rest.len() - arg.len();
        match arg.as_bytes().first() {
            Some(b'"' | b'\'') => computed(at, false),
            Some(b':') => match arg.as_bytes().get(1) {
                Some(b'"') => computed(at + 1, false),
                // A symbol is a literal name, whatever its case.
                _ => false,
            },
            Some(_) => true,
            None => false,
        }
    };
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for (at, _) in line.match_indices("constantize") {
        let after = at + "constantize".len();
        if !in_code(at) || bytes.get(after).is_some_and(|b| word(*b)) {
            continue;
        }
        let start = if line[..at].ends_with("safe_") {
            at - 5
        } else {
            at
        };
        // A definition, a symbol: only a call has a receiver.
        let Some(dot) = start.checked_sub(1).filter(|d| bytes[*d] == b'.') else {
            continue;
        };
        let defines = line[..start]
            .trim_start()
            .strip_prefix("def ")
            .is_some_and(|owner| !owner.contains([' ', '(']));
        if defines {
            continue;
        }
        let whole = if line[..dot].ends_with("Inflector") {
            computed_arg(after)
        } else if line[after..].starts_with('(') && !line[after..].starts_with("()") {
            // String#constantize takes no argument: this is another method.
            false
        } else {
            match dot.checked_sub(1).map(|r| bytes[r]) {
                Some(b'"' | b'\'') => computed(dot - 1, true),
                _ => true,
            }
        };
        if whole {
            return Some(line[start..after].to_string());
        }
    }
    for (at, _) in line.match_indices("const_get") {
        let after = at + "const_get".len();
        let before = at.checked_sub(1).map(|b| bytes[b]);
        if !in_code(at)
            || !matches!(bytes.get(after), Some(b'(' | b' '))
            || before.is_some_and(|b| word(b) || b == b':')
            || line[..at].trim_end().ends_with("def")
            || !computed_arg(after)
        {
            continue;
        }
        // Each constant a namespace lists, fetched: that listing's to bound.
        if line.contains("constants") {
            continue;
        }
        let receiver = match before {
            Some(b'.') => {
                let head = &line[..at - 1];
                let from = head
                    .rfind(|c: char| !(c.is_ascii_alphanumeric() || "_:.@".contains(c)))
                    .map_or(0, |i| i + 1);
                &head[from..]
            }
            _ => "",
        };
        let namespace = receiver.trim_start_matches("::");
        if is_constant_path(namespace) && !matches!(namespace, "Object" | "Module" | "Kernel") {
            continue;
        }
        return Some(match receiver {
            "" => "const_get".to_string(),
            receiver => format!("{receiver}.const_get"),
        });
    }
    None
}

/// `ObjectSpace.each_object(Class).select { |k| k < Base }`: a listing of
/// every class, which reaches `Base`'s subclasses when the line compares
/// with it (`Some(Some(base))`, as `Base.subclasses` does), and any class
/// when it names none (`Some(None)`). `each_object(Widget)` lists instances,
/// no class: `None`.
fn object_space_listing(line: &str) -> Option<Option<String>> {
    let (spans, code_end) = string_spans(line);
    let call = "ObjectSpace.each_object";
    let at = line[..code_end]
        .match_indices(call)
        .map(|(at, _)| at)
        .find(|at| !spans.iter().any(|(o, c)| o < at && at < c))?;
    let rest = &line[at + call.len()..];
    let arg = rest.strip_prefix('(').unwrap_or("").trim_start();
    let inner = arg.split(')').next().unwrap_or("").trim_end();
    // `each_object(Base.singleton_class)`: Base and its subclasses.
    if let Some(base) = inner
        .strip_suffix(".singleton_class")
        .filter(|base| is_constant_path(base))
    {
        return Some(Some(base.trim_start_matches("::").to_string()));
    }
    if !inner.is_empty() && !matches!(inner, "Class" | "Module" | "::Class" | "::Module") {
        return None;
    }
    // The first constant the rest of the line compares a class with: `k < Base`.
    let bytes = rest.as_bytes();
    for (i, _) in rest.match_indices('<') {
        if bytes.get(i + 1) == Some(&b'<') || i > 0 && bytes[i - 1] == b'<' {
            continue;
        }
        let after = rest[i + 1..].trim_start_matches(['=', ' ']);
        let base = after
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
            .next()
            .unwrap_or("");
        if is_constant_path(base) {
            return Some(Some(base.trim_start_matches("::").to_string()));
        }
    }
    Some(None)
}

/// `Widget.descendants`, `Widget.constants`: the constants a line sends one
/// of `calls` to, which list what they hold or what inherits them.
fn listings(line: &str, calls: &[&str]) -> Vec<String> {
    let mut found = Vec::new();
    for call in calls {
        let mut rest = line;
        while let Some(at) = rest.find(call) {
            let before = &rest[..at];
            let start = before
                .rfind(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == ':'))
                .map_or(0, |i| i + 1);
            let name = &before[start..];
            let after = &rest[at + call.len()..];
            let whole = !after.starts_with(|c: char| c.is_ascii_alphanumeric() || c == '_');
            if whole && is_constant_path(name) {
                found.push(name.trim_start_matches("::").to_string());
            }
            rest = after;
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str, text: &str) -> Named {
        let mut named = Named::default();
        named.read_ruby(path, text);
        named
    }

    #[test]
    fn a_whole_string_that_spells_a_constant_names_it() {
        let named = read(
            "a.rb",
            "has_one :x, class_name: 'Admin::Widget'\nputs \"Widget gone\"\n",
        );
        assert!(named.strings.contains_key("Admin::Widget"));
        assert!(!named.strings.keys().any(|k| k.contains("gone")));
    }

    #[test]
    fn a_constantized_interpolation_is_a_shape() {
        let named = read(
            "a.rb",
            "\"::Jobs::#{name.camelize}\".constantize\n\"#{x}\".constantize\n\"UI::#{x}\".constantize\n",
        );
        let shapes: Vec<&str> = named.shapes.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(shapes, ["Jobs::*", "UI::*"]);
    }

    #[test]
    fn symbols_and_keys_are_read_but_not_a_path_or_a_ternary() {
        let named = read(
            "a.rb",
            "validates :name, email_address: true\nx = a ? b :c\nFoo::Bar\n",
        );
        assert!(named.symbols.contains_key("emailaddress"));
        assert!(named.symbols.contains_key("name"));
        assert!(!named.symbols.contains_key("bar"));
    }

    #[test]
    fn an_association_names_its_class_by_convention() {
        let named = read(
            "a.rb",
            "  has_many :line_items\n  belongs_to :owner, polymorphic: true\n  included { has_one :\"#{name.underscore}_search_data\" }\n",
        );
        assert!(named.associated.contains_key("lineitem"));
        assert!(!named.associated.contains_key("owner"));
        let shapes: Vec<&str> = named.shapes.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(shapes, ["*SearchData"]);
    }

    #[test]
    fn a_local_that_holds_a_constant_lists_its_constants() {
        let named = read(
            "a.rb",
            "steps = Steps\nsteps.constants.map { |c| steps.const_get(c) }\n",
        );
        assert!(named.constants_listed.contains_key("Steps"));
        let named = read("a.rb", "jobs = Jobs\nall =\n  jobs\n    .constants\n");
        assert!(named.constants_listed.contains_key("Jobs"));
    }

    #[test]
    fn a_const_get_of_a_computed_name_names_its_namespace() {
        assert_eq!(
            computed_const_gets("Module.const_get(\"Regions\").const_get(region.upcase)"),
            ["Regions"]
        );
        assert_eq!(computed_const_gets("Widgets.const_get(name)"), ["Widgets"]);
        assert!(computed_const_gets("Widgets.const_get(:LIMIT)").is_empty());
        assert!(computed_const_gets("Object.const_get(name)").is_empty());
    }

    #[test]
    fn a_name_computed_whole_is_an_unscoped_lookup() {
        for (line, call) in [
            ("klass_name.constantize", "constantize"),
            ("\"#{ns}::#{name}\".safe_constantize", "safe_constantize"),
            ("ActiveSupport::Inflector.constantize(name)", "constantize"),
            ("Object.const_get(name)", "Object.const_get"),
            ("::Object.const_get name", "::Object.const_get"),
            ("self.class.const_get(\"#{x}\")", "self.class.const_get"),
            ("const_get(name.upcase)", "const_get"),
            ("type.to_s.camelize.constantize()", "constantize"),
        ] {
            assert_eq!(unscoped_lookup(line).as_deref(), Some(call), "{line}");
        }
        for line in [
            "\"Jobs::#{type.camelize}\".constantize",
            "'Admin::Widget'.constantize",
            "Regions.const_get(name)",
            "Object.const_get(:Widget)",
            "constants.map { |c| const_get(c) }",
            "def self.constantize(name)",
            "config.constantize('filter.map', :Fl)",
            "respond_to?(:constantize)",
            "x = 1 # name.constantize",
            "puts \"name.constantize\"",
        ] {
            assert_eq!(unscoped_lookup(line), None, "{line}");
        }
    }

    #[test]
    fn a_heredocs_text_and_a_block_comment_look_nothing_up() {
        let named = read(
            "a.rb",
            "X = <<~R\n  name.constantize\nR\n=begin\nname.constantize\n=end\nlist <<item\n",
        );
        assert!(named.unscoped.is_none() && named.computed.is_none());
        let named = read("a.rb", "class_eval <<~RUBY\n  name.constantize\nRUBY\n");
        assert_eq!(
            named.unscoped.map(|(call, _)| call).as_deref(),
            Some("constantize")
        );
    }

    #[test]
    fn object_space_lists_a_bases_subclasses_or_any_class() {
        let base = |line| object_space_listing(line);
        assert_eq!(
            base("ObjectSpace.each_object(Class).select { |k| k < Base }"),
            Some(Some("Base".into()))
        );
        assert_eq!(
            base("ObjectSpace.each_object(Jobs::Base.singleton_class).to_a"),
            Some(Some("Jobs::Base".into()))
        );
        assert_eq!(base("ObjectSpace.each_object(Class).to_a"), Some(None));
        assert_eq!(
            base("ObjectSpace.each_object(Widget) { |w| w.close }"),
            None
        );
        assert_eq!(base("puts 'ObjectSpace.each_object(Class)'"), None);
    }

    #[test]
    fn a_listing_of_subclasses_names_the_parent() {
        assert_eq!(
            listings("Rails::Engine.descendants.each", &[".descendants"]),
            ["Rails::Engine"]
        );
        assert!(listings("Widget.constants_for(x)", &[".constants"]).is_empty());
    }

    #[test]
    fn a_constant_read_on_a_value_is_dynamic_but_a_path_is_not() {
        let named = read(
            "a.rb",
            "permit(*self.class::PERMITTED)
Admin::Widget::LIMIT
klass.const_get(:SIZE)
",
        );
        assert!(named.dynamic.contains_key("PERMITTED"));
        assert!(named.dynamic.contains_key("SIZE"));
        assert!(!named.dynamic.contains_key("LIMIT") && !named.dynamic.contains_key("Widget"));
    }
}
