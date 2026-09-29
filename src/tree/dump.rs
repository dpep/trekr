//! Every class's and module's linearization in an indexed checkout, as text —
//! the equivalence check for any change to how chains are computed. Not a
//! unit test: it reads a real store and is run by hand, before and after.
//!
//! ```sh
//! TREKR_DB=… TREKR_LIN_DUMP=/abs/checkout TREKR_LIN_OUT=out.tsv \
//!   cargo test --release --lib dump_linearizations -- --ignored
//! ```
//!
//! `TREKR_LIN_ORDER=reverse` asks in the opposite order, which is how an
//! answer that depends on what was asked first shows itself.

use super::Tree;
use std::io::Write;

#[test]
#[ignore = "reads a real index; run by hand"]
fn dump_linearizations() {
    let (Some(root), Some(out)) = (
        std::env::var_os("TREKR_LIN_DUMP"),
        std::env::var_os("TREKR_LIN_OUT"),
    ) else {
        panic!("set TREKR_LIN_DUMP (checkout root) and TREKR_LIN_OUT (file)");
    };
    let store = crate::store::open_default().unwrap();
    let tree = Tree::build(&store, &root.to_string_lossy()).unwrap();
    let mut names = Vec::new();
    tree.names.for_each(|fqn, entry| {
        if matches!(entry.kind(), "class" | "module") {
            names.push(fqn.to_string());
        }
    });
    names.sort();
    if std::env::var("TREKR_LIN_ORDER").is_ok_and(|o| o == "reverse") {
        names.reverse();
    }
    let started = std::time::Instant::now();
    let mut rows: Vec<String> = names
        .iter()
        .map(|fqn| {
            let ancestry = tree.ancestors(fqn);
            let singleton: Vec<String> = tree
                .lookup_chain(fqn, true)
                .into_iter()
                .map(|(owner, s)| if s { format!("{owner}.") } else { owner })
                .collect();
            format!(
                "{fqn}\t{}\t{}\t{}",
                ancestry.chain.join(","),
                ancestry.unresolved.join(","),
                singleton.join(",")
            )
        })
        .collect();
    let memo = tree.ancestors.values();
    eprintln!(
        "{} names linearized in {} ms; {} chains memoized, {} of them closing a cycle",
        rows.len(),
        started.elapsed().as_millis(),
        memo.len(),
        memo.iter().filter(|m| m.cyclic).count()
    );
    rows.sort();
    let mut file = std::fs::File::create(out).unwrap();
    for row in rows {
        writeln!(file, "{row}").unwrap();
    }
}
