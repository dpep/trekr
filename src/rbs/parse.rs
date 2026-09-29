//! A reader for the part of RBS trekr needs: declarations and their
//! ancestry, and each method's overloads down to the shape of its
//! parameters and the one type it returns (DEC-240).
//!
//! Hand-written rather than the `ruby-rbs` bindings: the files come from
//! whichever rbs gem the app's Ruby carries, 3.x or 4.x, and a grammar pinned
//! to one version refuses a whole file for one member it does not know. This
//! one skips a member it cannot read and keeps the rest. Types are read only
//! as far as telling one class from anything else; everything else in them is
//! passed over by bracket.

/// A type, reduced to what a return is judged by.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Ty {
    /// `Foo`, `::Foo::Bar`, `Array[Elem]`, `_ToS`, `int`: a name as written.
    /// Which of a class, a type variable, an interface or an alias it is
    /// depends on the scope, and is the caller's to say.
    Name {
        path: String,
        args: Vec<Ty>,
    },
    Optional(Box<Ty>),
    Union(Vec<Ty>),
    /// `self`, `bool`, `void`, `untyped`, a literal, a tuple, a record, a
    /// proc, `singleton(…)`: never one class an instance is of.
    Other,
}

/// One overload's positional and keyword parameters: names where RBS gives
/// them, since a stub's `def` is written from them.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Function {
    pub(crate) required: Vec<Option<String>>,
    pub(crate) optional: Vec<Option<String>>,
    pub(crate) rest: Option<Option<String>>,
    pub(crate) trailing: Vec<Option<String>>,
    pub(crate) required_keywords: Vec<String>,
    pub(crate) optional_keywords: Vec<String>,
    pub(crate) rest_keywords: Option<Option<String>>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Overload {
    /// `[T]` before the parameters: names that are not classes here.
    pub(crate) type_params: Vec<String>,
    /// `None` for `(?)`, which says nothing of the parameters.
    pub(crate) function: Option<Function>,
    /// `Some(true)` for `{ … }`, `Some(false)` for `?{ … }`.
    pub(crate) block: Option<bool>,
    pub(crate) returns: Ty,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Vis {
    Public,
    Private,
}

/// `def x`, `def self.x`, or `def self?.x` — a module function, which is a
/// private instance method and a public singleton one.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Side {
    Instance,
    Singleton,
    Both,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Method {
    pub(crate) name: String,
    pub(crate) side: Side,
    /// Written on the member itself (`private def`), else the section's.
    pub(crate) visibility: Vis,
    pub(crate) overloads: Vec<Overload>,
    /// `| ...`: these overloads add to the ones already declared.
    pub(crate) overloading: bool,
    /// The comment directly above it, `#` and one space stripped per line.
    pub(crate) comment: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mixin {
    Include,
    Extend,
    Prepend,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Member {
    Method(Method),
    /// `alias new old`, on the singleton side when `self.` is written.
    Alias {
        new: String,
        old: String,
        singleton: bool,
        comment: String,
    },
    Mixin(Mixin, String),
    Constant(String),
    Decl(Decl),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Class,
    Module,
    Interface,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Decl {
    pub(crate) kind: Kind,
    /// As written: `Foo`, `Foo::Bar`, `::Foo`.
    pub(crate) name: String,
    pub(crate) type_params: Vec<String>,
    pub(crate) superclass: Option<String>,
    /// `class Foo = Bar`: another name for a class declared elsewhere.
    pub(crate) alias_of: Option<String>,
    /// `module Kernel : BasicObject`: what a module may be mixed into, and
    /// so what an alias in it may name.
    pub(crate) self_types: Vec<String>,
    pub(crate) members: Vec<Member>,
}

/// A file's top-level declarations and constants. A member that cannot be
/// read is skipped, and counted.
#[derive(Debug, Default)]
pub(crate) struct Parsed {
    pub(crate) members: Vec<Member>,
    pub(crate) skipped: usize,
}

pub(crate) fn parse(source: &str) -> Parsed {
    let tokens = lex(source);
    let mut parser = Parser {
        tokens,
        at: 0,
        skipped: 0,
    };
    let members = parser.body(true);
    Parsed {
        members,
        skipped: parser.skipped,
    }
}

// --- tokens -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    /// An identifier or constant, `?`/`!`/`=` suffix not included.
    Word(String),
    /// `@x`, `$x`, `@@x`.
    Var,
    Str,
    Sym,
    Int,
    Punct(&'static str),
    /// `%a{…}`, which says nothing trekr reads.
    Annotation,
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    /// Byte offset in the source, for method names read raw.
    start: usize,
    /// First on its line: where a member may begin.
    first: bool,
    /// The comment block ending on the line above, when this token opens one.
    comment: String,
}

const PUNCT: [&str; 21] = [
    "...", "::", "->", "**", "=>", "(", ")", "[", "]", "{", "}", ",", "|", "&", "?", "*", "^", ":",
    "<", "=", ".",
];

fn lex(source: &str) -> Vec<(Token, &str)> {
    let bytes = source.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    let mut line_start = true;
    // Nothing but whitespace on this line so far.
    let mut blank = true;
    let mut comment: Vec<&str> = Vec::new();
    // The comment lines seen since the last token, and whether a blank line
    // came after them — which detaches them from what follows.
    let mut blank_after_comment = false;
    while i < bytes.len() {
        let c = bytes[i];
        if c == b'\n' {
            if blank && !comment.is_empty() {
                blank_after_comment = true;
            }
            line_start = true;
            blank = true;
            i += 1;
            continue;
        }
        if c.is_ascii_whitespace() {
            i += 1;
            continue;
        }
        blank = false;
        if c == b'#' {
            let end = source[i..].find('\n').map_or(bytes.len(), |n| i + n);
            if line_start {
                if blank_after_comment {
                    comment.clear();
                    blank_after_comment = false;
                }
                let text = &source[i + 1..end];
                comment.push(text.strip_prefix(' ').unwrap_or(text));
            }
            i = end;
            continue;
        }
        let first = line_start;
        line_start = false;
        let attached = if first && !blank_after_comment {
            comment.join("\n")
        } else {
            String::new()
        };
        comment.clear();
        blank_after_comment = false;
        let start = i;
        let tok = if c == b'%' && bytes.get(i + 1) == Some(&b'a') {
            i = skip_annotation(bytes, i + 2);
            Tok::Annotation
        } else if c == b'"' || c == b'\'' {
            i = skip_string(bytes, i);
            Tok::Str
        } else if c == b'@' || c == b'$' {
            i += 1;
            if bytes.get(i) == Some(&b'-') {
                i += 1;
            }
            while i < bytes.len() && (bytes[i] == b'@' || is_word(bytes[i])) {
                i += 1;
            }
            // `$<`, `$!`, `$;`: a special global is one more character.
            if c == b'$' && i == start + 1 && i < bytes.len() {
                i += 1;
            }
            Tok::Var
        } else if c == b':' && bytes.get(i + 1).is_some_and(|n| symbol_start(*n)) {
            i += 1;
            if bytes[i] == b'"' || bytes[i] == b'\'' {
                i = skip_string(bytes, i);
            } else {
                while i < bytes.len()
                    && !bytes[i].is_ascii_whitespace()
                    && !b",)]}|".contains(&bytes[i])
                {
                    i += 1;
                }
            }
            Tok::Sym
        } else if c.is_ascii_digit()
            || (c == b'-' && bytes.get(i + 1).is_some_and(u8::is_ascii_digit))
        {
            i += 1;
            while i < bytes.len()
                && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_' || bytes[i] == b'.')
            {
                i += 1;
            }
            Tok::Int
        } else if is_word(c) {
            while i < bytes.len() && is_word(bytes[i]) {
                i += 1;
            }
            Tok::Word(source[start..i].to_string())
        } else if let Some(word) = quoted_word(&source[i..]) {
            // `` `type` ``: a keyword used as a parameter's name.
            i += word.len() + 2;
            Tok::Word(word.to_string())
        } else if let Some(p) = PUNCT.iter().find(|p| source[i..].starts_with(**p)) {
            i += p.len();
            Tok::Punct(p)
        } else {
            // A character no member here is made of (`!`, `~`, `` ` `` in an
            // operator's name, read raw): its own token, never matched.
            i += source[i..].chars().next().map_or(1, char::len_utf8);
            Tok::Punct("?!")
        };
        out.push((
            Token {
                tok,
                start,
                first,
                comment: attached,
            },
            source,
        ));
    }
    out
}

fn quoted_word(rest: &str) -> Option<&str> {
    let inner = rest.strip_prefix('`')?;
    let end = inner.find('`')?;
    let word = &inner[..end];
    (!word.is_empty() && word.bytes().all(is_word)).then_some(word)
}

fn is_word(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// After a `:`, what makes it a symbol literal rather than the colon of
/// `name: Type`, which is always followed by a space.
fn symbol_start(c: u8) -> bool {
    is_word(c) || b"\"'+-*/<>=!~[%&|^`".contains(&c)
}

fn skip_string(bytes: &[u8], at: usize) -> usize {
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() && bytes[i] != quote {
        if bytes[i] == b'\\' {
            i += 1;
        }
        i += 1;
    }
    (i + 1).min(bytes.len())
}

/// Past `%a{…}`, `%a(…)`, `%a[…]`, `%a<…>` or `%a|…|`.
fn skip_annotation(bytes: &[u8], at: usize) -> usize {
    let Some(&open) = bytes.get(at) else {
        return at;
    };
    let close = match open {
        b'{' => b'}',
        b'(' => b')',
        b'[' => b']',
        b'<' => b'>',
        other => other,
    };
    let mut depth = 0;
    let mut i = at;
    while i < bytes.len() {
        if bytes[i] == close && (depth == 1 || open == close) && i > at {
            return i + 1;
        }
        if bytes[i] == open {
            depth += 1;
        } else if bytes[i] == close {
            depth -= 1;
        }
        i += 1;
    }
    i
}

// --- parser -------------------------------------------------------------------

struct Parser<'s> {
    tokens: Vec<(Token, &'s str)>,
    at: usize,
    skipped: usize,
}

/// A member could not be read; the parser resynchronises at the next line
/// that starts one.
struct Unreadable;

type Read<T> = Result<T, Unreadable>;

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.at).map(|(t, _)| &t.tok)
    }

    fn peek_at(&self, ahead: usize) -> Option<&Tok> {
        self.tokens.get(self.at + ahead).map(|(t, _)| &t.tok)
    }

    fn token(&self) -> Option<&Token> {
        self.tokens.get(self.at).map(|(t, _)| t)
    }

    fn bump(&mut self) -> Option<Tok> {
        let tok = self.peek().cloned();
        self.at += 1;
        tok
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Some(Tok::Punct(q)) if *q == p)
    }

    fn is_word(&self, w: &str) -> bool {
        matches!(self.peek(), Some(Tok::Word(x)) if x == w)
    }

    fn eat(&mut self, p: &str) -> bool {
        let found = self.is(p);
        if found {
            self.at += 1;
        }
        found
    }

    fn expect(&mut self, p: &str) -> Read<()> {
        if self.eat(p) { Ok(()) } else { Err(Unreadable) }
    }

    fn word(&mut self) -> Read<String> {
        match self.bump() {
            Some(Tok::Word(w)) => Ok(w),
            _ => Err(Unreadable),
        }
    }

    /// Members until `end` (consumed) or, at the top level, the end of input.
    fn body(&mut self, top: bool) -> Vec<Member> {
        let mut members = Vec::new();
        let mut section = Vis::Public;
        while let Some(token) = self.token() {
            let comment = token.comment.clone();
            if self.is_word("end") {
                self.at += 1;
                if top {
                    continue;
                }
                return members;
            }
            let start = self.at;
            match self.member(&mut section, comment) {
                Ok(read) => members.extend(read),
                Err(Unreadable) => {
                    self.skipped += 1;
                    self.resync(start);
                }
            }
        }
        members
    }

    /// Skip to the next token that opens a line and could open a member.
    fn resync(&mut self, start: usize) {
        self.at = self.at.max(start + 1);
        while let Some(token) = self.token() {
            if token.first
                && matches!(&token.tok, Tok::Word(w) if MEMBER_WORDS.contains(&w.as_str()) || w.starts_with(|c: char| c.is_ascii_uppercase()))
            {
                return;
            }
            if token.first && matches!(token.tok, Tok::Annotation | Tok::Var) {
                return;
            }
            self.at += 1;
        }
    }

    fn member(&mut self, section: &mut Vis, comment: String) -> Read<Vec<Member>> {
        while matches!(self.peek(), Some(Tok::Annotation)) {
            self.at += 1;
        }
        let Some(tok) = self.peek().cloned() else {
            return Ok(Vec::new());
        };
        let Tok::Word(word) = tok else {
            if matches!(tok, Tok::Var) {
                // An instance or global variable's type: nothing to keep.
                self.at += 1;
                self.expect(":")?;
                self.ty()?;
                return Ok(Vec::new());
            }
            return Err(Unreadable);
        };
        match word.as_str() {
            "class" | "module" | "interface" => self.decl().map(|d| vec![Member::Decl(d)]),
            "def" => {
                self.at += 1;
                self.method(*section, comment)
                    .map(|m| vec![Member::Method(m)])
            }
            "private" | "public" => {
                self.at += 1;
                let vis = if word == "private" {
                    Vis::Private
                } else {
                    Vis::Public
                };
                // A section, unless the member it qualifies is on this line.
                let qualifies = self.token().is_some_and(|t| !t.first);
                if !qualifies {
                    *section = vis;
                    return Ok(Vec::new());
                }
                let mut own = vis;
                self.member(&mut own, comment)
            }
            "attr_reader" | "attr_writer" | "attr_accessor" => {
                self.at += 1;
                self.attr(&word, *section, comment)
            }
            "alias" => {
                self.at += 1;
                let (new, singleton) = self.alias_name()?;
                let (old, _) = self.alias_name()?;
                Ok(vec![Member::Alias {
                    new,
                    old,
                    singleton,
                    comment,
                }])
            }
            "include" | "extend" | "prepend" => {
                self.at += 1;
                let kind = match word.as_str() {
                    "include" => Mixin::Include,
                    "extend" => Mixin::Extend,
                    _ => Mixin::Prepend,
                };
                let name = self.type_name()?;
                self.type_args()?;
                Ok(vec![Member::Mixin(kind, name)])
            }
            "type" => {
                // `type name[T] = …`: an alias, which names no class.
                self.at += 1;
                self.type_name()?;
                self.type_params()?;
                self.expect("=")?;
                self.ty()?;
                Ok(Vec::new())
            }
            "use" | "self" => {
                // `use A::B as C`, and `self.@x: T`, a class instance
                // variable: nothing a stub says.
                self.skip_line();
                Ok(Vec::new())
            }
            w if w.starts_with(|c: char| c.is_ascii_uppercase()) || self.is("::") => {
                let name = self.type_name()?;
                self.expect(":")?;
                self.ty()?;
                Ok(vec![Member::Constant(name)])
            }
            _ => Err(Unreadable),
        }
    }

    /// Past the rest of this line, for a member whose content is never read.
    fn skip_line(&mut self) {
        self.at += 1;
        while self.token().is_some_and(|t| !t.first) {
            self.at += 1;
        }
    }

    fn decl(&mut self) -> Read<Decl> {
        let kind = match self.word()?.as_str() {
            "class" => Kind::Class,
            "module" => Kind::Module,
            _ => Kind::Interface,
        };
        let name = self.type_name()?;
        if self.eat("=") {
            let target = self.type_name()?;
            return Ok(Decl {
                kind,
                name,
                type_params: Vec::new(),
                superclass: None,
                alias_of: Some(target),
                self_types: Vec::new(),
                members: Vec::new(),
            });
        }
        let type_params = self.type_params()?;
        let mut superclass = None;
        if kind == Kind::Class && self.eat("<") {
            superclass = Some(self.type_name()?);
            self.type_args()?;
        }
        let mut self_types = Vec::new();
        if kind == Kind::Module && self.eat(":") {
            loop {
                self_types.push(self.type_name()?);
                self.type_args()?;
                if !self.eat(",") {
                    break;
                }
            }
        }
        let members = self.body(false);
        Ok(Decl {
            kind,
            name,
            type_params,
            superclass,
            alias_of: None,
            self_types,
            members,
        })
    }

    /// `Foo`, `::Foo::Bar`, `_Each`, `int`, as written.
    fn type_name(&mut self) -> Read<String> {
        let mut name = String::new();
        if self.eat("::") {
            name.push_str("::");
        }
        name.push_str(&self.word()?);
        while self.is("::") && matches!(self.peek_at(1), Some(Tok::Word(_))) {
            self.at += 1;
            name.push_str("::");
            name.push_str(&self.word()?);
        }
        Ok(name)
    }

    /// `[T, unchecked out U < Bound = Default]`: the names it introduces.
    fn type_params(&mut self) -> Read<Vec<String>> {
        let mut names = Vec::new();
        if !self.eat("[") {
            return Ok(names);
        }
        loop {
            while matches!(self.peek(), Some(Tok::Word(w)) if ["unchecked", "in", "out"].contains(&w.as_str()))
                && matches!(self.peek_at(1), Some(Tok::Word(_)))
            {
                self.at += 1;
            }
            names.push(self.word()?);
            if self.eat("<") {
                self.ty()?;
            }
            if self.eat("=") {
                self.ty()?;
            }
            if self.eat(",") {
                continue;
            }
            self.expect("]")?;
            return Ok(names);
        }
    }

    fn type_args(&mut self) -> Read<Vec<Ty>> {
        let mut args = Vec::new();
        if !self.eat("[") {
            return Ok(args);
        }
        if self.eat("]") {
            return Ok(args);
        }
        loop {
            args.push(self.ty()?);
            if self.eat(",") {
                continue;
            }
            self.expect("]")?;
            return Ok(args);
        }
    }

    /// A method's name, read from the source: an operator (`[]=`, `<=>`,
    /// `` ` ``) is not made of tokens that mean anything here.
    fn method_name(&mut self) -> Read<(Side, String)> {
        let (token, source) = self.tokens.get(self.at).ok_or(Unreadable)?;
        let mut rest = &source[token.start..];
        let side = if let Some(after) = rest.strip_prefix("self?.") {
            rest = after;
            Side::Both
        } else if let Some(after) = rest.strip_prefix("self.") {
            rest = after;
            Side::Singleton
        } else {
            Side::Instance
        };
        let (name, len) =
            if let Some(quoted) = rest.strip_prefix('`').filter(|q| !q.starts_with(':')) {
                let end = quoted.find('`').ok_or(Unreadable)?;
                (quoted[..end].to_string(), end + 2)
            } else {
                // Up to the `:` that is not part of the name: the next character
                // is a space (or the method type's first), never a second `:`.
                let bytes = rest.as_bytes();
                let end = (1..bytes.len())
                    .find(|&i| {
                        bytes[i] == b':' && bytes.get(i + 1) != Some(&b':') && bytes[i - 1] != b':'
                    })
                    .ok_or(Unreadable)?;
                (rest[..end].trim_end().to_string(), end)
            };
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err(Unreadable);
        }
        let consumed = token.start + (source[token.start..].len() - rest.len()) + len;
        while self
            .tokens
            .get(self.at)
            .is_some_and(|(t, _)| t.start < consumed)
        {
            self.at += 1;
        }
        Ok((side, name))
    }

    fn method(&mut self, visibility: Vis, comment: String) -> Read<Method> {
        let (side, name) = self.method_name()?;
        self.expect(":")?;
        let mut overloads = Vec::new();
        let mut overloading = false;
        loop {
            while matches!(self.peek(), Some(Tok::Annotation)) {
                self.at += 1;
            }
            if self.eat("...") {
                overloading = true;
            } else {
                overloads.push(self.method_type()?);
            }
            if !self.eat("|") {
                break;
            }
        }
        Ok(Method {
            name,
            side,
            visibility,
            overloads,
            overloading,
            comment,
        })
    }

    fn method_type(&mut self) -> Read<Overload> {
        let type_params = self.type_params()?;
        let function = if self.is("(") {
            self.params()?
        } else {
            Some(Function::default())
        };
        let block = if self.is("?") && matches!(self.peek_at(1), Some(Tok::Punct("{"))) {
            self.at += 1;
            self.block()?;
            Some(false)
        } else if self.is("{") {
            self.block()?;
            Some(true)
        } else {
            None
        };
        self.expect("->")?;
        let returns = self.optional()?;
        Ok(Overload {
            type_params,
            function,
            block,
            returns,
        })
    }

    /// `{ (params) [self: T] -> R }`, read past.
    fn block(&mut self) -> Read<()> {
        self.expect("{")?;
        if self.is("(") {
            self.params()?;
        }
        if self.eat("[") {
            self.skip_balanced("[", "]")?;
        }
        self.expect("->")?;
        self.optional()?;
        self.expect("}")
    }

    /// Past a group whose opener was just eaten.
    fn skip_balanced(&mut self, open: &str, close: &str) -> Read<()> {
        let mut depth = 1;
        while depth > 0 {
            match self.bump() {
                Some(Tok::Punct(p)) if p == open => depth += 1,
                Some(Tok::Punct(p)) if p == close => depth -= 1,
                Some(_) => {}
                None => return Err(Unreadable),
            }
        }
        Ok(())
    }

    /// `(Integer n, ?String s, *untyped, name: T, **opts)`; `None` for `(?)`.
    fn params(&mut self) -> Read<Option<Function>> {
        self.expect("(")?;
        if self.is("?") && matches!(self.peek_at(1), Some(Tok::Punct(")"))) {
            self.at += 2;
            return Ok(None);
        }
        let mut f = Function::default();
        if self.eat(")") {
            return Ok(Some(f));
        }
        loop {
            let optional = self.eat("?");
            if self.eat("**") {
                self.ty()?;
                f.rest_keywords = Some(self.param_name());
            } else if self.eat("*") {
                self.ty()?;
                f.rest = Some(self.param_name());
            } else if matches!(self.peek(), Some(Tok::Word(_)))
                && matches!(self.peek_at(1), Some(Tok::Punct(":")))
            {
                let key = self.word()?;
                self.at += 1;
                self.ty()?;
                self.param_name();
                if optional {
                    f.optional_keywords.push(key);
                } else {
                    f.required_keywords.push(key);
                }
            } else {
                self.ty()?;
                let name = self.param_name();
                if optional {
                    f.optional.push(name);
                } else if f.rest.is_some() {
                    f.trailing.push(name);
                } else {
                    f.required.push(name);
                }
            }
            // A trailing comma is allowed.
            if self.eat(",") && !self.is(")") {
                continue;
            }
            self.expect(")")?;
            return Ok(Some(f));
        }
    }

    /// A parameter's variable name, when one follows its type.
    fn param_name(&mut self) -> Option<String> {
        match self.peek() {
            Some(Tok::Word(w)) => {
                let w = w.clone();
                self.at += 1;
                Some(w)
            }
            _ => None,
        }
    }

    /// `attr_reader name: T`, `attr_accessor self.name(@ivar): T`: a reader,
    /// a writer, or both.
    fn attr(&mut self, macro_name: &str, visibility: Vis, comment: String) -> Read<Vec<Member>> {
        let mut side = Side::Instance;
        if self.is_word("self") && matches!(self.peek_at(1), Some(Tok::Punct("."))) {
            self.at += 2;
            side = Side::Singleton;
        }
        let name = self.word()?;
        if self.eat("(") {
            // `(@ivar)` or `()`: where it is stored.
            self.skip_balanced("(", ")")?;
        }
        self.expect(":")?;
        let returns = self.ty()?;
        let method = |name: String, required: Vec<Option<String>>| {
            Member::Method(Method {
                name,
                side,
                visibility,
                overloads: vec![Overload {
                    type_params: Vec::new(),
                    function: Some(Function {
                        required,
                        ..Function::default()
                    }),
                    block: None,
                    returns: returns.clone(),
                }],
                overloading: false,
                comment: comment.clone(),
            })
        };
        let reader = || method(name.clone(), Vec::new());
        let writer = || method(format!("{name}="), vec![Some(name.clone())]);
        Ok(match macro_name {
            "attr_reader" => vec![reader()],
            "attr_writer" => vec![writer()],
            _ => vec![reader(), writer()],
        })
    }

    /// `alias new old`, either side `self.`-qualified.
    fn alias_name(&mut self) -> Read<(String, bool)> {
        let (token, source) = self.tokens.get(self.at).ok_or(Unreadable)?;
        let text = &source[token.start..];
        let (singleton, rest) = match text.strip_prefix("self.") {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let len = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let name = rest[..len].to_string();
        if name.is_empty() {
            return Err(Unreadable);
        }
        let consumed = token.start + (text.len() - rest.len()) + len;
        while self
            .tokens
            .get(self.at)
            .is_some_and(|(t, _)| t.start < consumed)
        {
            self.at += 1;
        }
        Ok((name, singleton))
    }

    // --- types ----------------------------------------------------------------

    /// A type where `|` is a union.
    fn ty(&mut self) -> Read<Ty> {
        let first = self.intersection()?;
        if !self.is("|") {
            return Ok(first);
        }
        let mut members = vec![first];
        while self.eat("|") {
            members.push(self.intersection()?);
        }
        Ok(Ty::Union(members))
    }

    fn intersection(&mut self) -> Read<Ty> {
        let first = self.optional()?;
        if !self.is("&") {
            return Ok(first);
        }
        while self.eat("&") {
            self.optional()?;
        }
        Ok(Ty::Other)
    }

    /// A type where `|` ends it: a method's return, where the next `|` is
    /// its next overload. RBS wants a union there parenthesized.
    fn optional(&mut self) -> Read<Ty> {
        let mut ty = self.primary()?;
        while self.is("?") && !matches!(self.peek_at(1), Some(Tok::Punct("{"))) {
            self.at += 1;
            ty = Ty::Optional(Box::new(ty));
        }
        Ok(ty)
    }

    fn primary(&mut self) -> Read<Ty> {
        match self.peek().cloned().ok_or(Unreadable)? {
            Tok::Punct("(") => {
                self.at += 1;
                let inner = self.ty()?;
                self.expect(")")?;
                Ok(inner)
            }
            Tok::Punct("[") => {
                self.at += 1;
                self.skip_balanced("[", "]")?;
                Ok(Ty::Other)
            }
            Tok::Punct("{") => {
                self.at += 1;
                self.skip_balanced("{", "}")?;
                Ok(Ty::Other)
            }
            Tok::Punct("^") => {
                self.at += 1;
                if self.is("(") {
                    self.params()?;
                }
                if self.eat("[") {
                    self.skip_balanced("[", "]")?;
                }
                if self.is("?") && matches!(self.peek_at(1), Some(Tok::Punct("{"))) {
                    self.at += 1;
                }
                if self.is("{") {
                    self.block()?;
                }
                self.expect("->")?;
                self.optional()?;
                Ok(Ty::Other)
            }
            Tok::Str | Tok::Sym | Tok::Int => {
                self.at += 1;
                Ok(Ty::Other)
            }
            Tok::Punct("::") => self.named(),
            Tok::Word(w) => match w.as_str() {
                "singleton" if matches!(self.peek_at(1), Some(Tok::Punct("("))) => {
                    self.at += 2;
                    self.skip_balanced("(", ")")?;
                    Ok(Ty::Other)
                }
                "self" | "instance" | "class" | "bool" | "untyped" | "nil" | "top" | "bot"
                | "void" | "boolish" | "true" | "false"
                    if !matches!(self.peek_at(1), Some(Tok::Punct("::"))) =>
                {
                    self.at += 1;
                    Ok(Ty::Other)
                }
                _ => self.named(),
            },
            _ => Err(Unreadable),
        }
    }

    fn named(&mut self) -> Read<Ty> {
        let path = self.type_name()?;
        let args = self.type_args()?;
        Ok(Ty::Name { path, args })
    }
}

const MEMBER_WORDS: [&str; 15] = [
    "class",
    "module",
    "interface",
    "def",
    "end",
    "private",
    "public",
    "attr_reader",
    "attr_writer",
    "attr_accessor",
    "alias",
    "include",
    "extend",
    "prepend",
    "type",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn only_decl(source: &str) -> Decl {
        let parsed = parse(source);
        assert_eq!(parsed.skipped, 0, "{source}");
        match parsed.members.as_slice() {
            [Member::Decl(decl)] => decl.clone(),
            other => panic!("one declaration, not {other:?}"),
        }
    }

    fn methods(decl: &Decl) -> Vec<&Method> {
        decl.members
            .iter()
            .filter_map(|m| match m {
                Member::Method(m) => Some(m),
                _ => None,
            })
            .collect()
    }

    fn named(path: &str) -> Ty {
        Ty::Name {
            path: path.into(),
            args: Vec::new(),
        }
    }

    #[test]
    fn reads_a_class_its_ancestry_and_its_overloads() {
        let decl = only_decl(
            "%a{annotate:rdoc:source:from=array.c}\n\
             class Array[unchecked out Elem] < Object\n  include Enumerable[Elem]\n\n\
               # <!--\n  #   rdoc-file=array.c\n  #   - first -> object or nil\n  # -->\n\
               def first: %a{implicitly-returns-nil} () -> Elem\n           | (int n) -> ::Array[Elem]\n\n\
               def each: () { (Elem item) -> void } -> self\n          | () -> ::Enumerator[Elem, self]\nend\n",
        );
        assert_eq!((decl.kind, decl.name.as_str()), (Kind::Class, "Array"));
        assert_eq!(decl.type_params, ["Elem"]);
        assert_eq!(decl.superclass.as_deref(), Some("Object"));
        assert!(
            decl.members
                .contains(&Member::Mixin(Mixin::Include, "Enumerable".into()))
        );
        let methods = methods(&decl);
        let first = methods[0];
        assert_eq!(first.name, "first");
        assert!(
            first.comment.contains("- first -> object or nil"),
            "{:?}",
            first.comment
        );
        assert_eq!(first.overloads.len(), 2);
        assert_eq!(first.overloads[0].returns, named("Elem"));
        let second = first.overloads[1].function.as_ref().unwrap();
        assert_eq!(second.required, [Some("n".to_string())]);
        let each = methods[1];
        assert_eq!(each.overloads[0].block, Some(true));
        assert_eq!(each.overloads[0].returns, Ty::Other);
        assert_eq!(each.overloads[1].block, None);
    }

    #[test]
    fn reads_every_kind_of_parameter() {
        let decl = only_decl(
            "class W\n  def go: (Integer a, ?String b, *untyped rest, Symbol c, key: Integer, ?opt: bool, **untyped kw) ?{ () -> void } -> Integer?\n  def any: (?) -> untyped\nend\n",
        );
        let methods = methods(&decl);
        let go = &methods[0].overloads[0];
        let f = go.function.as_ref().unwrap();
        assert_eq!(f.required, [Some("a".to_string())]);
        assert_eq!(f.optional, [Some("b".to_string())]);
        assert_eq!(f.rest, Some(Some("rest".to_string())));
        assert_eq!(f.trailing, [Some("c".to_string())]);
        assert_eq!(f.required_keywords, ["key"]);
        assert_eq!(f.optional_keywords, ["opt"]);
        assert_eq!(f.rest_keywords, Some(Some("kw".to_string())));
        assert_eq!(go.block, Some(false));
        assert_eq!(go.returns, Ty::Optional(Box::new(named("Integer"))));
        assert_eq!(methods[1].overloads[0].function, None);
    }

    #[test]
    fn a_union_in_a_parameter_is_not_an_overload_and_a_parenthesized_return_is_one_type() {
        let decl = only_decl(
            "class W\n  def go: (Numeric | String real, ?exception: true) -> (String | Integer)\n       | () -> String\nend\n",
        );
        let go = methods(&decl)[0];
        assert_eq!(go.overloads.len(), 2);
        assert!(matches!(go.overloads[0].returns, Ty::Union(_)));
        assert_eq!(go.overloads[1].returns, named("String"));
    }

    #[test]
    fn reads_operators_module_functions_visibility_and_overloading() {
        let decl = only_decl(
            "module Kernel : BasicObject\n  def self?.puts: (*untyped) -> nil\n  def `: (String) -> String\n  private def secret: () -> void\n  def []=: (Integer, untyped) -> untyped\n  private\n  def hidden: () -> void\n  public\n  def <=>: (untyped) -> Integer?\n  def to_s: (Integer) -> String | ...\nend\n",
        );
        let found: Vec<(&str, Side, Vis)> = methods(&decl)
            .iter()
            .map(|m| (m.name.as_str(), m.side, m.visibility))
            .collect();
        assert_eq!(
            found,
            [
                ("puts", Side::Both, Vis::Public),
                ("`", Side::Instance, Vis::Public),
                ("secret", Side::Instance, Vis::Private),
                ("[]=", Side::Instance, Vis::Public),
                ("hidden", Side::Instance, Vis::Private),
                ("<=>", Side::Instance, Vis::Public),
                ("to_s", Side::Instance, Vis::Public),
            ]
        );
        assert!(methods(&decl)[6].overloading);
    }

    #[test]
    fn reads_attributes_aliases_constants_and_nested_declarations() {
        let parsed = parse(
            "ARGV: Array[String]\n$stdout: IO\nclass Mutex = Thread::Mutex\nmodule Process\n  CLOCK_REALTIME: Integer\n  class Status\n    attr_reader pid: Integer\n    attr_accessor self.count(@count): Integer\n    alias to_i pid\n    alias self.total self.count\n  end\n  type clock = Integer | Symbol\n  interface _Each[T]\n    def each: () { (T) -> void } -> void\n  end\nend\n",
        );
        assert_eq!(parsed.skipped, 0);
        assert_eq!(parsed.members[0], Member::Constant("ARGV".into()));
        let Member::Decl(alias) = &parsed.members[1] else {
            panic!()
        };
        assert_eq!(alias.alias_of.as_deref(), Some("Thread::Mutex"));
        let Member::Decl(process) = &parsed.members[2] else {
            panic!()
        };
        assert_eq!(
            process.members[0],
            Member::Constant("CLOCK_REALTIME".into())
        );
        let Member::Decl(status) = &process.members[1] else {
            panic!()
        };
        let names: Vec<(&str, Side)> = methods(status)
            .iter()
            .map(|m| (m.name.as_str(), m.side))
            .collect();
        assert_eq!(
            names,
            [
                ("pid", Side::Instance),
                ("count", Side::Singleton),
                ("count=", Side::Singleton)
            ]
        );
        assert!(status.members.contains(&Member::Alias {
            new: "total".into(),
            old: "count".into(),
            singleton: true,
            comment: String::new()
        }));
        let Member::Decl(each) = &process.members[2] else {
            panic!()
        };
        assert_eq!(each.kind, Kind::Interface);
    }

    #[test]
    fn a_member_it_cannot_read_is_skipped_and_the_rest_kept() {
        let parsed = parse(
            "class W\n  def broken: (Integer -> String\n  def fine: () -> String\nend\nclass V\nend\n",
        );
        assert_eq!(parsed.skipped, 1);
        assert_eq!(parsed.members.len(), 2);
        let Member::Decl(w) = &parsed.members[0] else {
            panic!()
        };
        assert_eq!(methods(w)[0].name, "fine");
    }

    #[test]
    fn procs_records_tuples_literals_and_singletons_are_no_class() {
        let decl = only_decl(
            "class W\n  def a: () -> ^(Integer) -> String\n  def b: () -> { name: String }\n  def c: () -> [Integer, String]\n  def d: () -> :sym\n  def e: () -> singleton(W)\n  def f: () -> self\n  def g: () -> bool\nend\n",
        );
        assert!(
            methods(&decl)
                .iter()
                .all(|m| m.overloads[0].returns == Ty::Other)
        );
    }
}
