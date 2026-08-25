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
BIN = os.environ.get("TREKR_BIN") or os.path.join(ROOT, "target/release/trekr")
SAMPLE = int(os.environ.get("SAMPLE", "300"))
SEED = int(os.environ.get("SEED", "12"))

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def ask(site):
    """Ask trekr, distinguishing an empty answer from a dead process.

    `--def` aborted with a stack overflow on three real positions and the
    scorer recorded it as "missed", the same verdict as an honest "no name
    here". A crash is a different fact and gets its own one.
    """
    spec = f"{site['file']}:{site['line']}:{site['col']}"
    done = subprocess.run([BIN, "--def", spec, "--json"], capture_output=True, check=False)
    # A signal killed it. Negative on Unix, and the only genuinely alarming
    # outcome here.
    if done.returncode < 0:
        return "crashed"
    # trekr's own "could not serve" — for these sites, a file in no indexed
    # checkout (Ruby's stdlib). A defined answer, not an error, and counting it
    # as a crash is how three of them looked like a live P0.
    if done.returncode == 2:
        return "not-indexed"
    try:
        return json.loads(done.stdout)
    except json.JSONDecodeError:
        return "crashed"


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


def checkout_root(files):
    """Walk up from the traced files until a git checkout is found."""
    if not files:
        return "/dev/null"
    here = os.path.commonpath(files)
    while here != "/":
        if os.path.isdir(os.path.join(here, ".git")) or os.path.isfile(os.path.join(here, ".git")):
            return here
        here = os.path.dirname(here)
    return os.path.commonpath(files)


def candidate_rank(site, answer):
    """1-based position of the true definition among the ranked candidates.

    The number the ranking features are for: a residue answer is only useful
    if the right guess is near the top of the list a reader actually scans.
    """
    for i, candidate in enumerate(answer.get("candidates") or [], 1):
        if hits([candidate.get("site", {})], site):
            return i
    return None


def verdict(site, answer, app_root):
    """One call site's outcome, split so that distinct realities do not share a name.

    Two published numbers have already been wrong because this function mapped
    unlike things to one bucket: a dead process scored `missed`, and an answer
    in `db/schema.rb` scored `residue`. Each verdict below is a claim that the
    realities inside it are the same reality.
    """
    if answer in ("crashed", "not-indexed"):
        return answer
    if not answer or answer.get("reason") == "no name at this position":
        return "no-name"

    # Not an engine verdict at all: the harness put the column on a different
    # token than the one Ruby dispatched, so whatever trekr says is an answer
    # to a different question. Counting these as engine errors overstates the
    # error rate and hides a fixable defect in the gold set.
    if answer.get("name") and answer["name"] != site["method"]:
        return "column-mismatch"

    reported = answer.get("sites") or []
    if answer.get("status") == "resolved" or reported:
        if hits(reported, site):
            return "correct"
        if is_generated(site) and any(in_app(r.get("path"), app_root) for r in reported):
            return "declaration"
        # Right method, wrong location, is not the same failure as resolving to
        # a different method: one is a location bug, the other a resolution bug.
        if answer.get("owner") and answer["owner"] == site.get("owner"):
            return "right-owner-wrong-site"
        return "wrong"

    candidates = [c.get("site", {}) for c in answer.get("candidates") or []]
    if hits(candidates, site):
        return "residue-hit"
    if is_generated(site) and any(in_app(c.get("path"), app_root) for c in candidates):
        return "declaration-offered"
    # Two coverage gaps, not a coverage gap and a ranking gap. Session 20
    # raised the candidate cap to 500 and this bucket did not shrink by a
    # single site: the truth is not in the pool at all, so no ordering can
    # reach it. It differs from `nothing-known` only in that *something* with
    # that name was found.
    return "residue-truth-absent" if candidates else "residue-nothing-known"


def main(path):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    gold = [json.loads(line) for line in open(path)]
    gold = [g for g in gold if g.get("def_file", "").endswith(".rb")]
    # The *checkout* root, not the common prefix of the traced files: a
    # generated attribute is answered with `db/schema.rb`, which shares no
    # directory with `app/`, and scoring it against `app/` filed every one of
    # them as residue.
    app_root = os.environ.get("APP_ROOT") or checkout_root(
        [g["file"] for g in gold if g["scope"] == "app"]
    )

    # App sites are the point and there are few, so all of them are scored;
    # the gem floor is sampled, because each site costs a tree rebuild.
    app = [g for g in gold if g["scope"] == "app"]
    gems = [g for g in gold if g["scope"] != "app"]
    if SAMPLE and len(gems) > SAMPLE:
        random.Random(SEED).shuffle(gems)
        gems = gems[:SAMPLE]

    results = []
    ranks = []
    for i, site in enumerate(app + gems, 1):
        answer = ask(site)
        results.append((site, verdict(site, answer, app_root)))
        rank = candidate_rank(site, answer) if isinstance(answer, dict) else None
        if rank:
            ranks.append((site["scope"], rank))
        if i % 50 == 0:
            print(f"  … {i}/{len(app) + len(gems)}")

    order = [
        "correct",
        "residue-hit",
        "declaration",
        "declaration-offered",
        "right-owner-wrong-site",
        "residue-truth-absent",
        "residue-nothing-known",
        "wrong",
        "no-name",
        "not-indexed",
        "crashed",
        "column-mismatch",
    ]

    def report(label, rows):
        if not rows:
            return
        # A bad gold column is a defect in the harness, not an outcome for the
        # engine, so it is reported beside the table and kept out of its
        # denominator rather than counted as an error.
        harness = sum(1 for _, v in rows if v == "column-mismatch")
        scored = [r for r in rows if r[1] != "column-mismatch"]
        tally = collections.Counter(v for _, v in scored)
        total = len(scored) or 1
        print(f"\n{label} — {total} scored call sites")
        width = max(len(k) for k in order)
        for key in order:
            if key != "column-mismatch" and tally[key]:
                print(f"  {key:<{width}}  {tally[key]:>4}  {100 * tally[key] / total:>5.1f}%")
        found = tally["correct"] + tally["residue-hit"]
        print(f"  {'found the definition':<{width}}  {found:>4}  {100 * found / total:>5.1f}%")
        print(f"  {'confidently wrong':<{width}}  {tally['wrong']:>4}  "
              f"{100 * tally['wrong'] / total:>5.1f}%")
        if harness:
            print(f"  ({harness} excluded: the gold column names a different token)")

    app_rows = [(s, v) for s, v in results if s["scope"] == "app"]
    report("APP CODE", app_rows)
    report("  of which plain methods", [r for r in app_rows if not is_generated(r[0])])
    report("  of which Rails-generated", [r for r in app_rows if is_generated(r[0])])
    report("GEM CODE (the floor)", [(s, v) for s, v in results if s["scope"] != "app"])

    if ranks:
        print("\nranking quality, where the truth was offered as a candidate:")
        for label, want in (("app", "app"), ("gem", None)):
            rows = [r for scope, r in ranks if (scope == "app") == (want == "app")]
            if not rows:
                continue
            first = sum(1 for r in rows if r == 1)
            top3 = sum(1 for r in rows if r <= 3)
            mrr = sum(1 / r for r in rows) / len(rows)
            print(f"  {label:<4} {len(rows):>4} offered   #1 {100 * first / len(rows):>5.1f}%"
                  f"   top-3 {100 * top3 / len(rows):>5.1f}%   MRR {mrr:.3f}")

    misses = [
        (s, v)
        for s, v in app_rows
        if v
        in (
            "wrong",
            "right-owner-wrong-site",
            "no-name",
            "residue-truth-absent",
            "residue-nothing-known",
            "crashed",
            "not-indexed",
            "column-mismatch",
        )
    ]
    if misses:
        print("\nevery app-code miss, which is the part worth reading:")
        for site, why in sorted(misses, key=lambda r: (r[0]["file"], r[0]["line"])):
            where = site["file"].split("/app/")[-1]
            print(f"  {why:<8} {where}:{site['line']}:{site['col']:<3} {site['method']:<18}"
                  f" → {os.path.basename(site['def_file'])}:{site['def_line']}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/trekr-gold.ndjson")
