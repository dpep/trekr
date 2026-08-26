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
  ambiguous-wrong
              trekr answered `ambiguous`, listed the definitions it beat, and
              picked the wrong one. Not a confident error (DEC-027).
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
# Which checkout answers a position inside a gem. Unpinned, trekr picks the app
# that most recently indexed the gem — deterministic given a store, but it moves
# as you work, and that moved a published gem figure by three points between
# runs (session 23). A measurement pins it; the product does not (DEC-029).
CONTEXT = os.environ.get("CONTEXT")
# App sites are the point, so they were all scored when a corpus had 63 of
# them. A real app has thousands, and each site costs a process, so they are
# sampled too — with the same seed, so the sample is a fixed one.
APP_SAMPLE = int(os.environ.get("APP_SAMPLE", "0"))
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
    argv = [BIN, "--def", spec, "--json"]
    if CONTEXT:
        argv += ["--context", CONTEXT]
    done = subprocess.run(argv, capture_output=True, check=False)
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
# The line Ruby ran is the only thing this needs to look at. What replaced an
# allowlist of three Rails files: that enumeration was never a principle, it
# missed `define_method` and `delegate` generators entirely, and it is what made
# session 29's model callbacks unscoreable.
PLAIN_DEF = re.compile(r"^\s*def\s+[A-Za-z_\[]")


def truth_is_generated(site, cache={}):
    """Is the line Ruby ran a `def`, or something that *made* a method?

    A fact about the gold entry alone — it never looks at trekr's answer, which
    is what keeps it from excusing a wrong one.

    A `def` inside a `class_eval` heredoc is *generated*, and it does not always
    announce itself at the front: Rails writes `def build_#{name}(*args)`, whose
    first characters are an ordinary method name. Testing the keyword alone
    called two of those written definitions and turned two declaration answers
    into errors — which is what the one run beside the old test was for.
    """
    path, line = site.get("def_file", ""), int(site.get("def_line", 0))
    if path not in cache:
        try:
            cache[path] = open(path, encoding="utf-8", errors="replace").read().split("\n")
        except OSError:
            cache[path] = []
    lines = cache[path]
    text = lines[line - 1] if 0 < line <= len(lines) else ""
    if not PLAIN_DEF.match(text):
        return True
    return "#{" in text


def candidate_rank(site, answer):
    """1-based position of the true definition among the ranked candidates.

    The number the ranking features are for: a residue answer is only useful
    if the right guess is near the top of the list a reader actually scans.
    """
    for i, candidate in enumerate(answer.get("candidates") or [], 1):
        if hits([candidate.get("site", {})], site):
            return i
    return None


def verdict(site, answer):
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
        # trekr's own disclosure, corroborated on the gold side. Both halves
        # are needed: the label alone would excuse every wrong macro answer,
        # and the gold check alone cannot tell a declaration from a miss.
        if answer.get("kind") == "declaration" and truth_is_generated(site):
            return "declaration"
        # Right method, wrong location, is not the same failure as resolving to
        # a different method: one is a location bug, the other a resolution bug.
        if answer.get("owner") and answer["owner"] == site.get("owner"):
            return "right-owner-wrong-site"
        # The product's claim is about *confident* answers, and `ambiguous`
        # exists precisely to withhold that confidence (DEC-027). An answer
        # that named its competitors and pointed at the wrong one is a
        # different failure from one that stood behind a single site, so it
        # gets its own verdict rather than inflating the headline.
        if answer.get("status") == "ambiguous":
            return "ambiguous-wrong"
        return "wrong"

    candidates = [c.get("site", {}) for c in answer.get("candidates") or []]
    if hits(candidates, site):
        return "residue-hit"
    offered_kinds = [c.get("kind") for c in answer.get("candidates") or []]
    if "declaration" in offered_kinds and truth_is_generated(site):
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
    # Session 15 needed a checkout root here, to tell an in-app answer from a
    # gem one — a proxy for "is this a declaration" that trekr now answers
    # itself. Nothing needs the root any more, and `checkout_root` goes with it.
    # App sites are the point and there are few, so all of them are scored;
    # the gem floor is sampled, because each site costs a tree rebuild.
    app = [g for g in gold if g["scope"] == "app"]
    gems = [g for g in gold if g["scope"] != "app"]
    if APP_SAMPLE and len(app) > APP_SAMPLE:
        random.Random(SEED).shuffle(app)
        app = app[:APP_SAMPLE]
    if SAMPLE and len(gems) > SAMPLE:
        random.Random(SEED).shuffle(gems)
        gems = gems[:SAMPLE]

    results = []
    ranks = []
    for i, site in enumerate(app + gems, 1):
        answer = ask(site)
        results.append((site, verdict(site, answer)))
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
        "ambiguous-wrong",
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
    report("  of which the truth is a written `def`",
           [r for r in app_rows if not truth_is_generated(r[0])])
    # Broader than the old "Rails-generated" split, and more accurate: it now
    # catches `define_method`, `delegate`, and an app's own generators too.
    report("  of which the truth is generated",
           [r for r in app_rows if truth_is_generated(r[0])])
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
            "ambiguous-wrong",
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
