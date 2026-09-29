//! Parallel branches each pick "the next" testbed case and decision number, and
//! a merge can land two of the same. Nothing else notices: the testbed runs
//! both cases and the decisions read fine, until someone cites one by number.

use std::collections::BTreeMap;
use std::path::Path;

fn duplicates(numbers: impl Iterator<Item = (String, String)>) -> Vec<String> {
    let mut seen: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (number, name) in numbers {
        seen.entry(number).or_default().push(name);
    }
    seen.into_iter()
        .filter(|(_, names)| names.len() > 1)
        .map(|(number, names)| format!("{number}: {}", names.join(", ")))
        .collect()
}

#[test]
fn testbed_cases_have_distinct_numbers() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/testbed");
    let cases = std::fs::read_dir(&dir).unwrap().filter_map(|entry| {
        let name = entry.ok()?.file_name().into_string().ok()?;
        let (number, _) = name.split_once('-')?;
        number
            .chars()
            .all(|c| c.is_ascii_digit())
            .then(|| (number.to_string(), name.clone()))
    });
    let clashes = duplicates(cases);
    assert!(
        clashes.is_empty(),
        "testbed numbers used twice: {clashes:?}"
    );
}

#[test]
fn decisions_have_distinct_numbers() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/DECISIONS.md");
    let text = std::fs::read_to_string(path).unwrap();
    // `### DEC-n …` subsections revisit an entry on purpose; only the `## `
    // headings are the entries themselves.
    let headings = text.lines().filter_map(|line| {
        let rest = line.strip_prefix("## DEC-")?;
        let number: String = rest.chars().take_while(char::is_ascii_digit).collect();
        (!number.is_empty()).then(|| (number, line.to_string()))
    });
    let clashes = duplicates(headings);
    assert!(
        clashes.is_empty(),
        "decision numbers used twice: {clashes:?}"
    );
}
