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

import collections, json, os, random, re, subprocess, sys

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


# Methods Rails writes at boot. Runtime truth points at the *generator* —
# `attribute_methods.rb`, `association.rb`, `enum.rb` — because that is where
# the `define_method` ran. trekr points at the macro that caused it:
# `belongs_to :supplier`, `enum :status`, the schema column. Those are
# different answers to different questions, and the macro is the one a person
# wants, so these are scored as their own bucket rather than blended in.
GENERATED_OWNER = re.compile(r"Generated(Attribute|Association)Methods|Enum::EnumMethods")
GENERATOR_FILE = re.compile(r"/activerecord-[^/]+/lib/active_record/"
                            r"(attribute_methods\.rb|enum\.rb|associations/builder/association\.rb)$")


def is_generated(site):
    return bool(
        GENERATED_OWNER.search(site.get("owner", ""))
        or GENERATOR_FILE.search(site.get("def_file", ""))
    )


def in_app(path, app_root):
    return bool(path) and path.startswith(app_root) and "/gems/" not in path


def verdict(site, answer, app_root):
    if answer is None or answer.get("reason") == "no name at this position":
        return "missed"
    reported = answer.get("sites") or []
    if answer.get("status") == "resolved" or reported:
        if hits(reported, site):
            return "correct"
        # A generated method, answered with the declaration that generates it.
        if is_generated(site) and any(in_app(r.get("path"), app_root) for r in reported):
            return "declaration"
        return "wrong"
    candidates = [c.get("site", {}) for c in answer.get("candidates") or []]
    if hits(candidates, site):
        return "residue-hit"
    if is_generated(site) and any(in_app(c.get("path"), app_root) for c in candidates):
        return "declaration-offered"
    return "residue"


def main(path):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    gold = [json.loads(line) for line in open(path)]
    gold = [g for g in gold if g.get("def_file", "").endswith(".rb")]
    app_root = os.environ.get("APP_ROOT") or os.path.commonpath(
        [g["file"] for g in gold if g["scope"] == "app"] or ["/"]
    )

    # App sites are the point and there are few, so all of them are scored;
    # the gem floor is sampled, because each site costs a tree rebuild.
    app = [g for g in gold if g["scope"] == "app"]
    gems = [g for g in gold if g["scope"] != "app"]
    if SAMPLE and len(gems) > SAMPLE:
        random.Random(SEED).shuffle(gems)
        gems = gems[:SAMPLE]

    results = []
    for i, site in enumerate(app + gems, 1):
        results.append((site, verdict(site, ask(site), app_root)))
        if i % 50 == 0:
            print(f"  … {i}/{len(app) + len(gems)}")

    order = [
        "correct",
        "residue-hit",
        "declaration",
        "declaration-offered",
        "residue",
        "wrong",
        "missed",
    ]

    def report(label, rows):
        if not rows:
            return
        tally = collections.Counter(v for _, v in rows)
        total = len(rows)
        print(f"\n{label} — {total} call sites")
        width = max(len(k) for k in order)
        for key in order:
            if tally[key]:
                print(f"  {key:<{width}}  {tally[key]:>4}  {100 * tally[key] / total:>5.1f}%")
        found = tally["correct"] + tally["residue-hit"]
        print(f"  {'found the definition':<{width}}  {found:>4}  {100 * found / total:>5.1f}%")
        print(f"  {'confidently wrong':<{width}}  {tally['wrong']:>4}  "
              f"{100 * tally['wrong'] / total:>5.1f}%")

    app_rows = [(s, v) for s, v in results if s["scope"] == "app"]
    report("APP CODE", app_rows)
    report("  of which plain methods", [r for r in app_rows if not is_generated(r[0])])
    report("  of which Rails-generated", [r for r in app_rows if is_generated(r[0])])
    report("GEM CODE (the floor)", [(s, v) for s, v in results if s["scope"] != "app"])

    misses = [(s, v) for s, v in app_rows if v in ("wrong", "missed", "residue")]
    if misses:
        print("\nevery app-code miss, which is the part worth reading:")
        for site, why in sorted(misses, key=lambda r: (r[0]["file"], r[0]["line"])):
            where = site["file"].split("/app/")[-1]
            print(f"  {why:<8} {where}:{site['line']}:{site['col']:<3} {site['method']:<18}"
                  f" → {os.path.basename(site['def_file'])}:{site['def_line']}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/trekr-gold.ndjson")
