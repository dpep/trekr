//! The files outside Ruby that name a gem: config, the commands a process
//! manager or CI runs, a linter's plugin list.

use super::*;

/// The files outside Ruby that load a gem by name: config, the commands a
/// process manager or CI runs, a linter's plugin list.
pub(super) struct Texts {
    pub(super) files: Vec<TextFile>,
}

impl Texts {
    pub(super) fn read(root: &Path, manifests: &HashSet<String>) -> Texts {
        let wanted = |path: &str| {
            // Prose (an issue template naming a gem) loads nothing.
            if manifests.contains(path) || path.contains("/locales/") || path.ends_with(".md") {
                return false;
            }
            match path.contains('/') {
                false => ROOT_FILES.iter().any(|name| path.starts_with(name)),
                true => DIRS.iter().any(|dir| path.starts_with(dir)),
            }
        };
        let files = crate::scan::listed(root, &[])
            .into_iter()
            .filter(|path| wanted(path))
            .filter_map(|path| {
                let text = crate::scan::read_lossy(root.join(&path)).ok()?;
                Some(TextFile::new(path, text))
            })
            .collect();
        Texts { files }
    }

    /// The first file naming one of `tokens` — by its own name
    /// (`config/puma.rb`), or a word on a line that is not a comment.
    pub(super) fn naming(&self, tokens: &[(String, &'static str)]) -> Option<Named> {
        for file in &self.files {
            let path = &file.path;
            let stem = path
                .rsplit('/')
                .next()
                .unwrap_or(path)
                .split('.')
                .next()
                .unwrap_or_default()
                .to_ascii_lowercase();
            if path.starts_with("config/")
                && let Some((_, by)) = tokens.iter().find(|(t, _)| *t == stem)
            {
                return Some(Named {
                    by,
                    path: path.clone(),
                    line: 1,
                    text: path.clone(),
                });
            }
            for (line, searched) in &file.lines {
                if let Some((_, by)) = tokens.iter().find(|(t, _)| has_word(searched, t)) {
                    let text: String = file
                        .text
                        .lines()
                        .nth(*line as usize - 1)
                        .unwrap_or_default()
                        .trim()
                        .chars()
                        .take(80)
                        .collect();
                    return Some(Named {
                        by,
                        path: path.clone(),
                        line: *line,
                        text,
                    });
                }
            }
        }
        None
    }
}

/// One file's lines a name is looked for on, numbered: a Ruby file's string
/// and symbol literals (`:sidekiq`, `"opentelemetry/instrumentation/pg"`),
/// not its code, where `logger.debug?` is no mention of the debug gem; any
/// other file's lines but its comments.
pub(super) struct TextFile {
    pub(super) path: String,
    pub(super) text: String,
    pub(super) lines: Vec<(u32, String)>,
}

impl TextFile {
    pub(super) fn new(path: String, text: String) -> TextFile {
        let ruby = super::built::RUBY.iter().any(|ext| path.ends_with(ext))
            || matches!(
                path.rsplit('/').next().unwrap_or(&path),
                "Rakefile" | "Guardfile" | "Capfile"
            )
            || super::views::ruby_script(&path, &text);
        let lines = match ruby {
            true => literals(text.as_bytes()),
            false => text
                .lines()
                .enumerate()
                .filter(|(_, line)| {
                    let code = line.trim_start();
                    !code.starts_with('#') && !code.starts_with("//")
                })
                .map(|(at, line)| (at as u32 + 1, line.to_ascii_lowercase()))
                .collect(),
        };
        TextFile { path, text, lines }
    }
}

/// Each string and symbol literal in Ruby source, lowercased, with its
/// line; a symbol's with its colon.
pub(super) fn literals(source: &[u8]) -> Vec<(u32, String)> {
    struct Finder<'s> {
        source: &'s [u8],
        found: Vec<(u32, String)>,
    }
    impl Finder<'_> {
        fn push(&mut self, offset: usize, text: &[u8]) {
            let line = self.source[..offset.min(self.source.len())]
                .iter()
                .filter(|b| **b == b'\n')
                .count() as u32
                + 1;
            self.found
                .push((line, String::from_utf8_lossy(text).to_ascii_lowercase()));
        }
    }
    impl<'pr> ruby_prism::Visit<'pr> for Finder<'_> {
        fn visit_string_node(&mut self, node: &ruby_prism::StringNode<'pr>) {
            self.push(node.location().start_offset(), node.unescaped());
        }
        fn visit_symbol_node(&mut self, node: &ruby_prism::SymbolNode<'pr>) {
            let mut text = b":".to_vec();
            text.extend_from_slice(node.unescaped());
            self.push(node.location().start_offset(), &text);
        }
    }
    let parsed = ruby_prism::parse(source);
    let mut finder = Finder {
        source,
        found: Vec::new(),
    };
    ruby_prism::Visit::visit(&mut finder, &parsed.node());
    finder.found
}

/// At the checkout's root, by name or prefix.
pub(super) const ROOT_FILES: [&str; 26] = [
    "Procfile",
    "Rakefile",
    "Makefile",
    "Dockerfile",
    "docker-compose",
    "compose.y",
    "package.json",
    ".rubocop",
    ".rspec",
    "Guardfile",
    "Capfile",
    "config.ru",
    "app.json",
    ".gitlab-ci.yml",
    ".travis.yml",
    "lefthook.yml",
    ".lefthook.yml",
    ".overcommit.yml",
    ".pre-commit-config.yaml",
    ".haml-lint.yml",
    ".erb-lint.yml",
    ".erb_lint.yml",
    ".slim-lint.yml",
    ".standard.yml",
    ".simplecov",
    ".foreman",
];

/// Directories read whole: scripts, CI, config (less locales), rake tasks.
pub(super) const DIRS: [&str; 10] = [
    "bin/",
    "exe/",
    "script/",
    "scripts/",
    ".github/",
    ".circleci/",
    ".buildkite/",
    ".devcontainer/",
    "config/",
    "lib/tasks/",
];

/// Whether `word` is in `line` with no name character either side; `-`
/// counts as one, so `rubocop` is not in `rubocop-rails`, and so does a
/// `.` before it: `.yardoc` is a directory, not yard.
#[expect(
    clippy::string_slice,
    reason = "offsets are where an ASCII token was found, or just past it"
)]
pub(super) fn has_word(line: &str, word: &str) -> bool {
    let named = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    line.match_indices(word).any(|(at, _)| {
        !line[..at]
            .chars()
            .next_back()
            .is_some_and(|c| named(c) || c == '.')
            && !line[at + word.len()..].chars().next().is_some_and(named)
    })
}

/// The words that name a gem outside Ruby, lowercased, each with what it
/// is: its name and spellings of it, what Bundler requires for it, the
/// adapter a database config names it by, and its commands.
pub(super) fn tokens(
    declaration: &Declaration,
    executables: &[String],
) -> Vec<(String, &'static str)> {
    let name = declaration.name.to_ascii_lowercase();
    let mut words: Vec<(String, &'static str)> = vec![
        (name.clone(), "config"),
        (name.replace('-', "_"), "config"),
        (name.replace('-', "/"), "config"),
        (name.replace(['-', '_'], ""), "config"),
    ];
    if let Require::Paths(paths) = &declaration.require {
        words.extend(paths.iter().map(|p| (p.to_ascii_lowercase(), "config")));
    }
    // The adapter names Active Record's database configs use.
    let adapters: &[&str] = match name.as_str() {
        "pg" => &["postgresql", "postgis", "postgres"],
        "activerecord-postgis-adapter" => &["postgis"],
        "mysql2" => &["mysql2"],
        "trilogy" => &["trilogy"],
        "sqlite3" => &["sqlite3"],
        _ => &[],
    };
    words.extend(adapters.iter().map(|a| (a.to_string(), "config")));
    words.extend(
        executables
            .iter()
            .map(|e| (e.to_ascii_lowercase(), "command")),
    );
    words.retain(|(w, _)| w.len() >= 2);
    words.dedup();
    words
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_word_is_whole_and_a_hyphen_joins_names() {
        assert!(has_word("web: bundle exec puma -c config/puma.rb", "puma"));
        assert!(has_word("require: rubocop-rails", "rubocop-rails"));
        assert!(!has_word("require: rubocop-rails", "rubocop"));
        assert!(!has_word("adapter: postgresql_x", "postgresql"));
        assert!(has_word("  adapter: postgresql", "postgresql"));
        assert!(!has_word("rm -rf .yardoc doc", "yardoc"));
        assert!(!has_word("cache: .yard", "yard"));
    }
}
