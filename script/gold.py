#!/usr/bin/env python3
"""Score `trekr --def` against what Ruby actually dispatched.

    script/trace_gold.rb   # in a bootable app, writes the gold set
    script/gold.py /tmp/trekr-gold.ndjson

Every accuracy number this project has published came from a hand audit of a
sample. This one comes from runtime truth: a TracePoint recorded, for a few
hundred real call sites, which method Ruby resolved to and where it is defined
(`script/trace_gold.rb`). Here we ask trekr the same question and compare.

**Verdicts.**
  correct     trekr resolved, and its site is the file and line Ruby used.
  wrong       trekr resolved, and pointed somewhere else. The costly one.
  residue-hit trekr declined to resolve but offered the truth as a candidate.
  residue     trekr declined and did not offer it.
  missed      trekr found no name at that position at all.

Wrong and residue are not the same failure and are never blended: a ranked "I
am not sure, here are eight" is the product working as designed, and a
confident wrong answer is not.

Scoring spawns one process per site and each rebuilds the tree, so a sample is
the default. `SAMPLE=0` scores everything.
"""

import collections, json, os, random, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target/release/trekr")
SAMPLE = int(os.environ.get("SAMPLE", "300"))
SEED = int(os.environ.get("SEED", "12"))

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def ask(site):
    spec = f"{site['file']}:{site['line']}:{site['col']}"
    out = subprocess.run(
        [BIN, "--def", spec, "--json"], capture_output=True, check=False
    ).stdout
    try:
        return json.loads(out)
    except json.JSONDecodeError:
        return None


def hits(answer_sites, site):
    """Does any reported site name the file and line Ruby really used?"""
    for reported in answer_sites or []:
        path = reported.get("path") or ""
        if os.path.realpath(path) != os.path.realpath(site["def_file"]):
            continue
        # A definition's recorded line can differ by one from Ruby's, which
        # reports the `def` keyword's line for some macro-defined methods.
        if abs(int(reported.get("line", -99)) - int(site["def_line"])) <= 1:
            return True
    return False


def verdict(site, answer):
    if answer is None or answer.get("reason") == "no name at this position":
        return "missed"
    if answer.get("status") == "resolved" or answer.get("sites"):
        return "correct" if hits(answer.get("sites"), site) else "wrong"
    candidates = [c.get("site", {}) for c in answer.get("candidates") or []]
    return "residue-hit" if hits(candidates, site) else "residue"


def main(path):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    gold = [json.loads(line) for line in open(path)]
    # Only call sites — a gold entry whose method Ruby found in C has no Ruby
    # definition to point at.
    gold = [g for g in gold if g.get("def_file", "").endswith(".rb")]
    if SAMPLE and len(gold) > SAMPLE:
        random.Random(SEED).shuffle(gold)
        gold = gold[:SAMPLE]

    tally = collections.Counter()
    by_scope = collections.defaultdict(collections.Counter)
    wrong = []
    for i, site in enumerate(gold, 1):
        result = verdict(site, ask(site))
        tally[result] += 1
        by_scope[site["scope"]][result] += 1
        if result == "wrong" and len(wrong) < 10:
            wrong.append(site)
        if i % 50 == 0:
            print(f"  … {i}/{len(gold)}")

    total = sum(tally.values()) or 1
    print(f"\nscored {total} call sites against runtime truth\n")
    order = ["correct", "residue-hit", "residue", "wrong", "missed"]
    width = max(len(k) for k in order)
    for key in order:
        n = tally[key]
        print(f"  {key:<{width}}  {n:>5}  {100 * n / total:>5.1f}%")
    print(
        f"\n  found the true definition (resolved or offered): "
        f"{100 * (tally['correct'] + tally['residue-hit']) / total:.1f}%"
    )
    print(f"  confidently wrong: {100 * tally['wrong'] / total:.1f}%")

    print("\nby scope:")
    for scope, counts in sorted(by_scope.items()):
        n = sum(counts.values()) or 1
        print(
            f"  {scope:<6} {n:>5} sites   correct {100 * counts['correct'] / n:>5.1f}%"
            f"   +offered {100 * counts['residue-hit'] / n:>5.1f}%"
            f"   wrong {100 * counts['wrong'] / n:>5.1f}%"
        )

    if wrong:
        print("\na few confident misses, which are the ones worth reading:")
        for site in wrong:
            print(
                f"  {site['file']}:{site['line']}:{site['col']}  {site['method']}"
                f"  → truth {site['def_file']}:{site['def_line']}"
            )


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/trekr-gold.ndjson")
