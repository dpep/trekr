//! Active Support's English inflections (`active_support/inflections.rb`),
//! which Rails uses to name a resource's controller, a `has_many`'s model and
//! an `enum`'s mapping. Ported rule for rule, in the order Active Support
//! tries them — irregulars, then the last rule defined first — including the
//! answers that are wrong English (`specimens` → `speciman`), since those are
//! the names Rails looks for. A project's own `inflections.rb` is not read.

const UNCOUNTABLE: [&str; 10] = [
    "equipment",
    "information",
    "rice",
    "money",
    "species",
    "series",
    "fish",
    "sheep",
    "jeans",
    "police",
];

/// Defined in this order, so tried last first.
const IRREGULAR: [(&str, &str); 6] = [
    ("person", "people"),
    ("man", "men"),
    ("child", "children"),
    ("sex", "sexes"),
    ("move", "moves"),
    ("zombie", "zombies"),
];

/// `\bword\Z`: a snake_case name's last part is not a word on its own, so
/// `big_fish` is countable, as it is in Rails.
fn uncountable(name: &str) -> bool {
    UNCOUNTABLE.iter().any(|word| {
        name.strip_suffix(word)
            .is_some_and(|before| !before.ends_with(|c: char| c.is_alphanumeric() || c == '_'))
    })
}

/// The irregular pair `name` ends in either spelling of, as `to` spells it.
fn irregular(name: &str, to_plural: bool) -> Option<String> {
    IRREGULAR.iter().rev().find_map(|&(one, many)| {
        let to = if to_plural { many } else { one };
        [one, many]
            .iter()
            .find_map(|from| name.strip_suffix(from))
            .map(|stem| format!("{stem}{to}"))
    })
}

/// `"person".pluralize`.
pub(crate) fn plural(name: &str) -> String {
    if name.is_empty() || uncountable(name) {
        return name.to_string();
    }
    if let Some(word) = irregular(name, true) {
        return word;
    }
    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| name.ends_with(s));
    let cut = |n: usize, to: &str| format!("{}{to}", &name[..name.len() - n]);
    let before = |n: usize| name[..name.len() - n].chars().last();
    match name {
        _ if ends(&["quiz"]) => format!("{name}zes"),
        "ox" => "oxen".to_string(),
        "oxen" | "mice" | "lice" => name.to_string(),
        "mouse" | "louse" => cut(4, "ice"),
        _ if ends(&["matrix", "matrex", "vertix", "vertex", "indix", "index"]) => cut(2, "ices"),
        _ if ends(&["x", "ch", "ss", "sh"]) => format!("{name}es"),
        _ if name.ends_with('y')
            && (name.ends_with("quy") || before(1).is_some_and(|c| !"aeiouy".contains(c))) =>
        {
            cut(1, "ies")
        }
        _ if ends(&["hive"]) => format!("{name}s"),
        _ if name.ends_with("fe") && before(2).is_some_and(|c| c != 'f') => cut(2, "ves"),
        _ if ends(&["lf", "rf"]) => cut(1, "ves"),
        _ if ends(&["sis"]) => cut(3, "ses"),
        _ if ends(&["ta", "ia"]) => name.to_string(),
        _ if ends(&["tum", "ium"]) => cut(2, "a"),
        _ if ends(&["buffalo", "tomato"]) => format!("{name}es"),
        _ if ends(&["bus"]) => format!("{name}es"),
        _ if ends(&["alias", "status"]) => format!("{name}es"),
        _ if ends(&["octopi", "viri"]) => name.to_string(),
        _ if ends(&["octopus", "virus"]) => cut(2, "i"),
        "axis" | "testis" => cut(2, "es"),
        _ if name.ends_with('s') => name.to_string(),
        _ => format!("{name}s"),
    }
}

/// `"people".singularize`.
pub(crate) fn singular(name: &str) -> String {
    if name.is_empty() || uncountable(name) {
        return name.to_string();
    }
    if let Some(word) = irregular(name, false) {
        return word;
    }
    let ends = |suffixes: &[&str]| suffixes.iter().any(|s| name.ends_with(s));
    let cut = |n: usize, to: &str| format!("{}{to}", &name[..name.len() - n]);
    let before = |n: usize| name[..name.len() - n].chars().last();
    let sis = |stems: &[&str]| {
        stems.iter().any(|stem| {
            [format!("{stem}sis"), format!("{stem}ses")]
                .iter()
                .any(|w| name.ends_with(w.as_str()))
        })
    };
    match name {
        _ if ends(&["databases"]) => cut(1, ""),
        _ if ends(&["quizzes"]) => cut(3, ""),
        _ if ends(&["matrices"]) => cut(3, "x"),
        _ if ends(&["vertices", "indices"]) => cut(4, "ex"),
        // `^(ox)en`, unanchored at the end, as Active Support writes it.
        _ if name.starts_with("oxen") => format!("ox{}", &name[4..]),
        _ if ends(&["aliases", "statuses"]) => cut(2, ""),
        _ if ends(&["alias", "status"]) => name.to_string(),
        _ if ends(&["octopi", "viri"]) => cut(1, "us"),
        _ if ends(&["octopus", "virus"]) => name.to_string(),
        "axis" | "axes" => "axis".to_string(),
        _ if ends(&["crisis", "crises", "testis", "testes"]) => cut(2, "is"),
        _ if ends(&["shoes"]) => cut(1, ""),
        _ if ends(&["oes"]) => cut(2, ""),
        _ if ends(&["buses"]) => cut(2, ""),
        _ if ends(&["bus"]) => name.to_string(),
        "mice" | "lice" => cut(3, "ouse"),
        _ if ends(&["xes", "ches", "sses", "shes"]) => cut(2, ""),
        _ if ends(&["movies"]) => cut(1, ""),
        _ if ends(&["series"]) => name.to_string(),
        _ if name.ends_with("ies")
            && (name.ends_with("quies") || before(3).is_some_and(|c| !"aeiouy".contains(c))) =>
        {
            cut(3, "y")
        }
        _ if name.ends_with("ves") && before(3).is_some_and(|c| "lr".contains(c)) => cut(3, "f"),
        _ if ends(&["tives", "hives"]) => cut(1, ""),
        _ if name.ends_with("ves") && before(3).is_some_and(|c| c != 'f') => cut(3, "fe"),
        _ if sis(&[
            "analy", "ba", "diagno", "parenthe", "progno", "synop", "the",
        ]) =>
        {
            cut(2, "is")
        }
        _ if ends(&["ta", "ia"]) => cut(1, "um"),
        _ if ends(&["news"]) => name.to_string(),
        _ if ends(&["ss"]) => name.to_string(),
        _ if name.ends_with('s') => cut(1, ""),
        _ => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each pair checked against Active Support 8.1's `pluralize`.
    #[test]
    fn pluralizes_as_active_support_does() {
        for (one, many) in [
            ("settings", "settings"),
            ("news", "news"),
            ("person", "people"),
            ("people", "people"),
            ("salesperson", "salespeople"),
            ("status", "statuses"),
            ("gadget", "gadgets"),
            ("category", "categories"),
            ("box", "boxes"),
            ("wife", "wives"),
            ("analysis", "analyses"),
            ("medium", "media"),
            ("sheep", "sheep"),
            ("big_fish", "big_fishes"),
            ("quiz", "quizzes"),
            ("day", "days"),
            ("segment", "segments"),
            ("branch", "branches"),
        ] {
            assert_eq!(plural(one), many, "plural({one})");
        }
    }

    /// Each pair checked against Active Support 8.1's `singularize`.
    #[test]
    fn singularizes_as_active_support_does() {
        for (many, one) in [
            ("widgets", "widget"),
            ("people", "person"),
            ("salespeople", "salesperson"),
            ("women", "woman"),
            ("children", "child"),
            ("responses", "response"),
            ("purchases", "purchase"),
            ("courses", "course"),
            ("releases", "release"),
            ("licenses", "license"),
            ("cases", "case"),
            ("phases", "phase"),
            ("houses", "house"),
            ("movies", "movie"),
            ("categories", "category"),
            ("boxes", "box"),
            ("addresses", "address"),
            ("statuses", "status"),
            ("buses", "bus"),
            ("aliases", "alias"),
            ("analyses", "analysis"),
            ("diagnoses", "diagnosis"),
            ("bases", "basis"),
            ("databases", "database"),
            ("wives", "wife"),
            ("halves", "half"),
            ("archives", "archive"),
            ("objectives", "objective"),
            ("heroes", "hero"),
            ("shoes", "shoe"),
            ("media", "medium"),
            ("matrices", "matrix"),
            ("indices", "index"),
            ("quizzes", "quiz"),
            ("mice", "mouse"),
            ("oxen", "ox"),
            ("octopi", "octopus"),
            ("crises", "crisis"),
            ("axes", "axis"),
            ("news", "news"),
            ("series", "series"),
            ("species", "species"),
            ("equipment", "equipment"),
            ("moves", "move"),
            ("zombies", "zombie"),
            ("person", "person"),
            ("class", "class"),
            ("days", "day"),
        ] {
            assert_eq!(singular(many), one, "singular({many})");
        }
    }
}
