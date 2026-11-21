//! Ruby the index does not read, read here for what names a gem.

use super::*;

/// Ruby the index does not read: a file with no extension that Prism
/// parses without an error (`bin/setup`, `Vagrantfile`, `fastlane/Fastfile`),
/// and files Ruby in all but name — a Builder, Arbre or Jb view, a Thor
/// file or Capistrano task, an `.irbrc` — whose requires, constants and
/// calls count too. A Slim view, which nothing here parses, counts by its
/// words.
pub(super) struct Scripts {
    pub(super) files: Vec<(String, Vec<u8>, crate::core::Facts)>,
    /// Each Slim view's words: a constant path or a method name, by line.
    pub(super) words: Vec<(String, Vec<(u32, String)>)>,
}

impl Scripts {
    pub(super) fn read(root: &Path) -> Scripts {
        let mut scripts = Scripts {
            files: Vec::new(),
            words: Vec::new(),
        };
        for path in crate::scan::listed(root, &[]) {
            let name = path.rsplit('/').next().unwrap_or(&path);
            let plain = PLAIN_RUBY.iter().any(|ext| name.ends_with(ext))
                || PLAIN_RUBY_NAMES.contains(&name);
            let slim = name.ends_with(".slim");
            // One the index reads (`Rakefile`) is in the store already.
            if !plain && !slim && (name.contains('.') || crate::scan::is_ruby(&path)) {
                continue;
            }
            let Ok(raw) = crate::scan::read_source(root.join(&path)) else {
                continue;
            };
            if slim {
                let ruby = crate::extract::slim::slim_ruby(&String::from_utf8_lossy(&raw));
                scripts.words.push((path, words(&ruby)));
                continue;
            }
            // A file with no extension is Ruby when it parses as Ruby: a
            // shebang, a Ruby DSL's own name (`Fastfile`, `Steepfile`), or
            // neither. Text that is not Ruby hardly ever parses.
            if !plain && (raw.contains(&0) || !parses_as_code(&raw)) {
                continue;
            }
            let facts = crate::extract::extract(&raw);
            scripts.files.push((path, raw, facts));
        }
        scripts
    }

    pub(super) fn const_refs(&self) -> impl Iterator<Item = crate::store::ConstRefRow> + '_ {
        let parsed = self.files.iter().flat_map(|(path, _, facts)| {
            facts.const_refs.iter().map(|r| crate::store::ConstRefRow {
                path: path.clone(),
                name: r.name.clone(),
                nesting: r.nesting.clone(),
                line: r.pos.line,
            })
        });
        let worded = self.words.iter().flat_map(|(path, words)| {
            words
                .iter()
                .filter(|(_, word)| word.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(|(line, word)| crate::store::ConstRefRow {
                    path: path.clone(),
                    name: word.clone(),
                    nesting: Vec::new(),
                    line: *line,
                })
        });
        parsed.chain(worded)
    }

    pub(super) fn sources(&self) -> impl Iterator<Item = (String, Vec<u8>)> + '_ {
        self.files
            .iter()
            .map(|(path, raw, _)| (path.clone(), raw.clone()))
    }

    pub(super) fn calls(&self) -> impl Iterator<Item = (String, String)> + '_ {
        let parsed = self.files.iter().flat_map(|(path, _, facts)| {
            facts.calls.iter().map(|c| (c.name.clone(), path.clone()))
        });
        let worded = self.words.iter().flat_map(|(path, words)| {
            words
                .iter()
                .filter(|(_, word)| !word.starts_with(|c: char| c.is_ascii_uppercase()))
                .map(|(_, word)| (word.clone(), path.clone()))
        });
        parsed.chain(worded)
    }

    /// The paths a Slim view's `require "…"` names, each with its line.
    pub(super) fn word_requires(&self) -> impl Iterator<Item = (String, String, u32)> + '_ {
        self.words.iter().flat_map(|(path, words)| {
            words.iter().filter_map(|(line, word)| {
                let required = word.strip_prefix("require ")?;
                Some((path.clone(), required.to_string(), *line))
            })
        })
    }
}

/// Plain Ruby the index leaves out, by extension or by name.
pub(super) const PLAIN_RUBY: [&str; 7] = [
    ".builder", ".arb", ".jb", ".axlsx", ".prawn", ".thor", ".cap",
];

pub(super) const PLAIN_RUBY_NAMES: [&str; 2] = [".irbrc", ".pryrc"];

/// Whether text with no extension is Ruby: it parses, and holds something
/// prose does not — a literal, a block, parentheses, a receiver, an
/// assignment, a definition. A `TODO` reading "remove Tomlrb" parses as
/// two bare calls on a constant, and is prose.
pub(super) fn parses_as_code(raw: &[u8]) -> bool {
    struct Code(bool);
    impl Code {
        fn check(&mut self, node: &ruby_prism::Node<'_>) {
            use ruby_prism::Node;
            let worded = match node {
                Node::ProgramNode { .. }
                | Node::StatementsNode { .. }
                | Node::ArgumentsNode { .. }
                | Node::ConstantReadNode { .. }
                | Node::LocalVariableReadNode { .. } => true,
                Node::CallNode { .. } => node.as_call_node().is_some_and(|call| {
                    call.receiver().is_none()
                        && call.block().is_none()
                        && call.opening_loc().is_none()
                }),
                _ => false,
            };
            self.0 |= !worded;
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Code {
        fn visit_branch_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
        fn visit_leaf_node_enter(&mut self, node: ruby_prism::Node<'pr>) {
            self.check(&node);
        }
    }
    let parsed = ruby_prism::parse(raw);
    if parsed.errors().next().is_some() {
        return false;
    }
    let mut code = Code(false);
    ruby_prism::Visit::visit(&mut code, &parsed.node());
    code.0
}

/// A text's words, by line: each constant path (`Diffy::Diff`), method
/// name (`paginate`, `valid?`), and `require <path>` for a literal require.
#[expect(
    clippy::string_slice,
    reason = "a byte scan stops only at ASCII bytes, which are char boundaries"
)]
pub(super) fn words(text: &str) -> Vec<(u32, String)> {
    let mut found = Vec::new();
    for (at, line) in text.lines().enumerate() {
        let line_no = at as u32 + 1;
        let bytes = line.as_bytes();
        let named = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
        let mut i = 0;
        while i < bytes.len() {
            let b = bytes[i];
            if !(b.is_ascii_alphabetic() || b == b'_') || (i > 0 && named(bytes[i - 1])) {
                i += 1;
                continue;
            }
            let start = i;
            while i < bytes.len() && named(bytes[i]) {
                i += 1;
            }
            if b.is_ascii_uppercase() {
                // `Diffy::Diff`: the whole path, as Ruby looks it up.
                while bytes[i..].starts_with(b"::")
                    && bytes.get(i + 2).is_some_and(|c| c.is_ascii_uppercase())
                {
                    i += 2;
                    while i < bytes.len() && named(bytes[i]) {
                        i += 1;
                    }
                }
            } else if matches!(bytes.get(i), Some(b'?' | b'!')) {
                i += 1;
            }
            // `Foo::Bar`'s `Bar` is no constant of its own. Bytes: a
            // multi-byte character may end just before `start`.
            if start >= 2 && bytes[start - 2..start] == *b"::" {
                continue;
            }
            let word = &line[start..i];
            found.push((line_no, word.to_string()));
            if word == "require" {
                let rest = line[i..].trim_start_matches([' ', '(']);
                if let Some(quote) = rest.chars().next().filter(|c| matches!(c, '"' | '\''))
                    && let Some((required, _)) = rest[1..].split_once(quote)
                {
                    found.push((line_no, format!("require {required}")));
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_with_no_extension_is_ruby_when_it_holds_code() {
        for code in [
            "Vagrant.configure(\"2\") do |config|\nend\n",
            "lane :beta do\n  build_app\nend\n",
            "git_url \"https://example.com/certs\"\ntype \"development\"\n",
            "require \"widget\"\nWidget.run\n",
            "x = 1\n",
        ] {
            assert!(parses_as_code(code.as_bytes()), "{code}");
        }
        for prose in [
            "remove Tomlrb\nbump Rails\n",
            "Retriable\n",
            "  \n",
            "TODO: fix it.\n",
        ] {
            assert!(!parses_as_code(prose.as_bytes()), "{prose}");
        }
    }
}
