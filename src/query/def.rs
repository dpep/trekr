//! What a definition is asked of at a position, beneath both fronts:
//! `--def FILE:LINE:COL` and Go to Definition take their [`Subject`] from
//! [`subject`], in one order (DEC-036, DEC-524), and each answers it its own
//! way — the CLI from disk and its index, the editor from its buffers and
//! resident tree. The two read a position differently, and only there: a
//! column names a character, and an editor's caret is read onto one first
//! ([`super::position::caret_reads`]).

use super::position::{self, Under};
use super::require::Require;
use crate::core::{Facts, Named, Pos, TemplateRef};
use crate::resolve::vars::{Occurrence, Vars};
use crate::tree::Tree;
use std::ops::Deref;
use std::path::Path;

/// The files a `render` or `extends` names, checkout-relative, and the class
/// its object was given (DEC-524).
pub(crate) struct Reached {
    pub(crate) files: Vec<String>,
    pub(crate) class: Option<String>,
}

/// What a definition is asked of.
pub(crate) enum Subject {
    /// A `require` string: the file it loads (DEC-053).
    Require(Require),
    /// A template a `render` or `extends` names. Its files are empty only
    /// where no variable is written there to answer instead.
    Template(Reached),
    /// A variable. `unreached` is the template a `render` written as it
    /// names, which reaches no file: said beside the variable's answer when
    /// that finds no value either.
    Variable {
        occurrence: Occurrence,
        unreached: Option<Reached>,
    },
    /// A definition, constant or call written there.
    Fact(Under),
    /// `super` with no fact behind it: its method's owner is not one the
    /// source names.
    Super,
    /// A symbol no rule reads as a method's name: a key or a value (DEC-343).
    Symbol(String),
    /// No name written there. The CLI snaps to the nearest on the line; an
    /// editor never does (DEC-036).
    Nothing,
}

/// What the character at `at` asks a definition of. A `require` string
/// comes first, wherever in it the character is, then a template, then a
/// variable, then a fact: `render @widgets` opens the partial, and a
/// variable is not a call. `requires` are the file's
/// ([`super::require::requires_in`]); `reach` finds a template's files,
/// `None` where the file is in no checkout to look in.
pub(crate) fn subject(
    facts: &Facts,
    source: &[u8],
    vars: &Vars,
    requires: &[Require],
    at: Pos,
    reach: impl FnOnce(&TemplateRef) -> anyhow::Result<Option<Reached>>,
) -> anyhow::Result<Subject> {
    let Pos { line, col } = at;
    if let Some(offset) = position::offset_of(source, line, col)
        && let Some(require) = requires.iter().find(|r| r.span.contains(&offset))
    {
        return Ok(Subject::Require(require.clone()));
    }
    let variable = || position::variable_at(facts, source, vars, line, col).cloned();
    if let Some(template) = position::template_at(facts, line, col)
        && let Some(reached) = reach(template)?
    {
        // One that reaches no file leaves the variable it is written as to
        // answer: `render item`, its class unknown.
        return Ok(match (reached.files.is_empty(), variable()) {
            (true, Some(occurrence)) => Subject::Variable {
                occurrence,
                unreached: Some(reached),
            },
            _ => Subject::Template(reached),
        });
    }
    if let Some(occurrence) = variable() {
        return Ok(Subject::Variable {
            occurrence,
            unreached: None,
        });
    }
    if let Some(under) = position::at_facts(facts, line, col) {
        return Ok(Subject::Fact(under));
    }
    if position::word_at(source, line, col).as_deref() == Some("super") {
        return Ok(Subject::Super);
    }
    let Some((name, named)) = symbol_at(source, line, col) else {
        return Ok(Subject::Nothing);
    };
    // The whole written symbol answers as its name does: on the `:` of
    // `send(:go)` or `before_save :go`, the call it sends.
    Ok(position::at_facts(facts, named.line, named.col)
        .map_or(Subject::Symbol(name), Subject::Fact))
}

/// The symbol literal written over a character, its leading `:` included,
/// and where its name starts.
fn symbol_at(source: &[u8], line: u32, col: u32) -> Option<(String, Pos)> {
    crate::extract::symbol_literals(source)
        .into_iter()
        .find(|(_, pos, len)| {
            pos.line == line && opened_at(source, *pos) <= col && col < pos.col + *len as u32
        })
        .map(|(name, pos, _)| (name, pos))
}

/// The column a symbol whose name starts at `name` opens at: its leading
/// `:` (or `:"`), where it has one. A key's colon trails (`name:`), so
/// the character before the name is not the symbol's.
fn opened_at(source: &[u8], name: Pos) -> u32 {
    let Some(start) = position::offset_of(source, name.line, name.col) else {
        return name.col;
    };
    let before = &source[..start];
    let unquoted = before
        .strip_suffix(b"\"")
        .or_else(|| before.strip_suffix(b"'"))
        .unwrap_or(before);
    match unquoted.strip_suffix(b":") {
        Some(opened) => name.col - (start - opened.len()) as u32,
        None => name.col,
    }
}

/// The files a template reaches from the file at `relative` in the checkout
/// at `root`. Only an object's partial needs the tree, to name its class.
pub(crate) fn reach<T: Deref<Target = Tree>>(
    template: &TemplateRef,
    facts: &Facts,
    root: &Path,
    relative: &str,
    tree: impl FnOnce() -> anyhow::Result<T>,
) -> anyhow::Result<Reached> {
    let class = match &template.names {
        Named::Object { value, .. } => {
            let tree = tree()?;
            crate::resolve::views::value_class(&tree, facts, value, template.pos, relative)
        }
        _ => None,
    };
    let files =
        crate::tree::views::template_files(root, relative, &template.names, class.as_deref());
    Ok(Reached { files, class })
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = concat!(
        "class Widget < Base\n",          // 1
        "  def go(list, fields:)\n",      // 2
        "    a = list[0]\n",              // 3
        "    -a + b\n",                   // 4
        "    call(fields:, on: :save)\n", // 5
        "    render \"nope\"\n",          // 6
        "    render @widgets\n",          // 7
        "    render a\n",                 // 8
        "    Other::Thing.new\n",         // 9
        "  end\n",                        // 10
        "end\n",                          // 11
        "super\n",                        // 12
        "x(:\"quoted\")\n",               // 13
        "class Cb\n",                     // 14
        "  before_save :go\n",            // 15
        "  def run = send(:'go')\n",      // 16
        "end\n",                          // 17
        "y = :\"quoted\"\n",              // 18
        "require_relative \"lib/x\"\n",   // 19
    );

    /// A subject as a line of text, to compare and to read in a failure.
    fn said(subject: &Subject) -> String {
        match subject {
            Subject::Require(require) => format!("require {}", require.path),
            Subject::Template(reached) => format!("template {:?}", reached.files),
            Subject::Variable {
                occurrence,
                unreached,
            } => match unreached {
                Some(_) => format!("variable {} (no template)", occurrence.name),
                None => format!("variable {}", occurrence.name),
            },
            Subject::Fact(Under::Definition(def)) => format!("definition {}", def.name),
            Subject::Fact(Under::Constant(reference)) => format!("constant {}", reference.name),
            Subject::Fact(Under::Call(call)) => format!("call {}", call.name),
            Subject::Super => "super".to_string(),
            Subject::Symbol(name) => format!("symbol {name}"),
            Subject::Nothing => "nothing".to_string(),
        }
    }

    /// Both readings of every position in [`SOURCE`]: a column on the
    /// character, and an editor's caret just before it.
    struct Readings {
        facts: Facts,
        vars: Vars,
        requires: Vec<Require>,
    }

    impl Readings {
        fn new() -> Readings {
            let source = SOURCE.as_bytes();
            let facts = crate::extract::extract(source);
            let vars = crate::resolve::vars::of_file(source, &facts.strings);
            let requires = crate::query::require::requires_in(source);
            Readings {
                facts,
                vars,
                requires,
            }
        }

        fn at(&self, at: Pos) -> String {
            // `@widgets` reaches its partial; the rest reach none.
            let reach = |template: &TemplateRef| {
                let files = match template.pos.line {
                    7 => vec!["app/views/widgets/_widget.html.erb".to_string()],
                    _ => Vec::new(),
                };
                Ok(Some(Reached { files, class: None }))
            };
            let subject = subject(
                &self.facts,
                SOURCE.as_bytes(),
                &self.vars,
                &self.requires,
                at,
                reach,
            );
            said(&subject.expect("no tree is asked"))
        }

        fn column(&self, line: u32, col: u32) -> String {
            self.at(Pos { line, col })
        }

        fn caret(&self, line: u32, col: u32) -> String {
            let read = position::caret_reads(SOURCE.as_bytes(), &self.vars, Pos { line, col });
            self.at(read)
        }
    }

    #[test]
    fn each_position_asks_one_subject_in_one_order() {
        let readings = Readings::new();
        // (line, col, a column on that character, the caret just before it)
        let cases = [
            (1, 7, "definition Widget", "definition Widget"),
            (1, 16, "constant Base", "constant Base"),
            (2, 7, "definition go", "definition go"),
            (3, 4, "nothing", "nothing"),
            (3, 5, "variable a", "variable a"),
            (3, 9, "variable list", "variable list"),
            // A column names `[`; a caret there sits just past `list`.
            (3, 13, "call []", "variable list"),
            (4, 5, "call -@", "call -@"),
            (4, 6, "variable a", "variable a"),
            (4, 7, "variable a", "variable a"),
            (4, 8, "call +", "call +"),
            (4, 10, "call b", "call b"),
            // Shorthand `fields:` reads the local, not a symbol.
            (5, 10, "variable fields", "variable fields"),
            (5, 24, "symbol save", "symbol save"),
            // A symbol's leading `:` is the symbol; the character before a
            // key, whose colon trails, is not.
            (5, 23, "symbol save", "symbol save"),
            (5, 9, "nothing", "nothing"),
            (5, 18, "nothing", "nothing"),
            (6, 13, "template []", "template []"),
            (
                7,
                12,
                "template [\"app/views/widgets/_widget.html.erb\"]",
                "template [\"app/views/widgets/_widget.html.erb\"]",
            ),
            // Just past `@widgets`: the line's end, where a caret reads the
            // argument and a column the variable it only touches.
            (
                7,
                20,
                "variable @widgets",
                "template [\"app/views/widgets/_widget.html.erb\"]",
            ),
            (
                8,
                12,
                "variable a (no template)",
                "variable a (no template)",
            ),
            (9, 5, "constant Other", "constant Other"),
            (9, 12, "constant Other::Thing", "constant Other::Thing"),
            (9, 18, "call new", "call new"),
            (12, 1, "super", "super"),
            // A symbol sent as a method's name is that call, its opening
            // included.
            (13, 3, "call quoted", "call quoted"),
            (13, 4, "call quoted", "call quoted"),
            (13, 5, "call quoted", "call quoted"),
            (15, 15, "call go", "call go"),
            (15, 16, "call go", "call go"),
            (16, 18, "call go", "call go"),
            (16, 19, "call go", "call go"),
            (16, 20, "call go", "call go"),
            (18, 5, "symbol quoted", "symbol quoted"),
            (18, 6, "symbol quoted", "symbol quoted"),
            // A require string is its file, quotes included; the call is the
            // call.
            (19, 1, "call require_relative", "call require_relative"),
            (19, 18, "require lib/x", "require lib/x"),
            (19, 22, "require lib/x", "require lib/x"),
            (19, 24, "require lib/x", "require lib/x"),
        ];
        for (line, col, column, caret) in cases {
            assert_eq!(readings.column(line, col), column, "column {line}:{col}");
            assert_eq!(
                readings.caret(line, col),
                caret,
                "caret before {line}:{col}"
            );
        }
    }

    /// Wherever the caret is, the editor answers what a column on one of
    /// the two characters beside it answers: the one to its right, unless
    /// a variable ends at the caret.
    #[test]
    fn a_caret_answers_what_a_column_beside_it_answers() {
        let readings = Readings::new();
        for (index, text) in SOURCE.lines().enumerate() {
            let line = index as u32 + 1;
            for col in 1..=text.len() as u32 + 1 {
                let caret = readings.caret(line, col);
                let right = readings.column(line, col);
                let left = (col > 1).then(|| readings.column(line, col - 1));
                assert!(
                    caret == right || Some(&caret) == left.as_ref(),
                    "caret before {line}:{col} answered `{caret}`, a column `{right}` or `{left:?}`"
                );
            }
        }
    }
}
