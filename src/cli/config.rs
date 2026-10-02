//! Method names a checkout's YAML config writes: a caller `--dead` cannot
//! see, said per row as a view's is (DEC-402).
//!
//! Not a reading of the config. A name here is a scalar — `generator:
//! pay_schedule_resources`, or the method of `Pkg::Sanitizer.clean_uid` —
//! that code elsewhere may hand to `public_send` or `constantize`. Evidence
//! enough for a caveat, never for a caller.

use std::collections::HashMap;
use std::path::Path;
use std::process::Command;

#[derive(Default)]
pub(super) struct Config {
    /// Each name, with the first file (relative to the checkout) and line
    /// that writes it.
    names: HashMap<String, (String, usize)>,
}

impl Config {
    /// Every YAML file git knows in the checkout at `root`, less the ones
    /// that are text or machine output rather than config.
    pub(super) fn read(root: &Path) -> Config {
        let Ok(out) = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-z", "--", "*.yml", "*.yaml"])
            .output()
        else {
            return Config::default();
        };
        let mut config = Config::default();
        for path in out.stdout.split(|b| *b == 0).filter(|p| !p.is_empty()) {
            let path = String::from_utf8_lossy(path).into_owned();
            if not_config(&path) {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(root.join(&path)) else {
                continue;
            };
            for (at, line) in text.lines().enumerate() {
                if let Some(name) = scalar(line).and_then(method_named) {
                    config
                        .names
                        .entry(name.to_string())
                        .or_insert_with(|| (path.clone(), at + 1));
                }
            }
        }
        config
    }

    /// Where `name` is first written, as `path:line`.
    pub(super) fn naming(&self, name: &str) -> Option<String> {
        self.names
            .get(name)
            .map(|(path, line)| format!("{path}:{line}"))
    }
}

/// A locale's text — on discourse 4,243 of its 4,452 YAML files — and a
/// lockfile name words, not methods.
fn not_config(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    let file = parts.pop().unwrap_or_default();
    parts.contains(&"locales")
        || file
            .rsplit_once('.')
            .is_some_and(|(stem, _)| stem == "lock" || stem.ends_with("-lock"))
}

/// The scalar a line sets: a mapping's value or a list item, unquoted, less
/// a trailing comment.
fn scalar(line: &str) -> Option<&str> {
    let line = line.trim();
    if line.starts_with('#') {
        return None;
    }
    let item = line.strip_prefix("- ").unwrap_or(line);
    let value = match item.split_once(": ") {
        Some((_, value)) => value,
        None if item.len() < line.len() => item,
        None => return None,
    };
    let value = value.split(" #").next().unwrap_or_default().trim();
    Some(value.trim_matches(|c| c == '"' || c == '\''))
}

/// The method a scalar names: the tail of `Const.method`, or a bare name
/// shaped as only a method is — snake_case with an underscore, or ending in
/// `?`/`!`. A bare word (`summary`, `moderation`) is English as often as it
/// is a method, and on discourse every one that matched a candidate was.
fn method_named(value: &str) -> Option<&str> {
    if let Some((receiver, method)) = value.rsplit_once('.') {
        let constant = receiver
            .trim_start_matches("::")
            .split("::")
            .all(|part| part.starts_with(|c: char| c.is_ascii_uppercase()) && is_word(part));
        return (constant && is_method(method)).then_some(method);
    }
    let core = value.trim_end_matches(['?', '!']);
    let shaped = core.trim_matches('_').contains('_') || core.len() < value.len();
    (is_method(value) && shaped).then_some(value)
}

fn is_method(name: &str) -> bool {
    let core = name.strip_suffix(['?', '!']).unwrap_or(name);
    core.starts_with(|c: char| c.is_ascii_lowercase() || c == '_') && is_word(core)
}

fn is_word(text: &str) -> bool {
    !text.is_empty() && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scalar_names_a_method_only_when_shaped_as_one() {
        let named = |line| scalar(line).and_then(method_named);
        assert_eq!(
            named("  generator: pay_schedule_resources # x"),
            Some("pay_schedule_resources")
        );
        assert_eq!(
            named("  sanitizer: \"Pkg::Foo.sanitize_uid\""),
            Some("sanitize_uid")
        );
        assert_eq!(named("- ready?"), Some("ready?"));
        assert_eq!(named("  kind: summary"), None);
        assert_eq!(named("  url: https://example.com/a_b"), None);
        assert_eq!(named("  host: example.com"), None);
        assert_eq!(named("# generator: pay_schedule"), None);
        assert_eq!(named("payroll:"), None);
    }

    #[test]
    fn locales_and_lockfiles_are_not_config() {
        assert!(not_config("config/locales/en.yml"));
        assert!(not_config("plugins/chat/config/locales/client.en.yml"));
        assert!(not_config("pnpm-lock.yaml"));
        assert!(!not_config("config/site_settings.yml"));
        assert!(!not_config("config/clock.yml"));
    }
}
