#!/usr/bin/env python3
"""Why is the true definition not among trekr's candidates?

    script/absent.py /tmp/trekr-gold.ndjson

DEC-028 closed the door on reaching these by ranking: raising the candidate cap
from 8 to 500 did not recover a single site, so the truth is not in the pool at
all. This asks the next question — *what are these methods* — because the four
possible answers want four different responses:

  unindexed-source   the file Ruby ran is in no indexed checkout. A coverage
                     gap with a known fix (index it) or a known reason.
  not-extracted      the file is indexed, but nothing trekr extracted from it
                     sits at that line. `define_method`, `class_eval`, a macro
                     family we do not model. Tractable extraction work.
  not-reached        trekr *has* the definition and did not reach it from this
                     call. A lookup or ancestry gap, not a coverage one.
  core-stub          Ruby's own core, which trekr answers from a vendored stub
                     rather than the real source.

The first is a question about setup, the second is work, the third is a bug,
and the fourth is a stated limit. Blending them is how "27 % residue" stayed
one undifferentiated number for five sessions.
"""

import collections, json, os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("TREKR_BIN") or os.path.join(ROOT, "target/release/trekr")
SAMPLE = int(os.environ.get("SAMPLE", "0"))

# Lines that define a method without a `def`. Subdividing `not-extracted` by
# what the definition *looks like* is what turns it from a number into work.
SHAPES = [
    ("define_method", re.compile(r"\bdefine_method\b|\bdefine_singleton_method\b")),
    ("class_eval / module_eval", re.compile(r"\b(class_eval|module_eval|instance_eval)\b")),
    ("attr_* macro", re.compile(r"\battr_(reader|writer|accessor)\b")),
    ("delegation macro", re.compile(r"\b(delegate|def_delegator|def_delegators)\b")),
    ("alias", re.compile(r"\b(alias_method|alias)\b")),
    ("Struct / Data member", re.compile(r"\b(Struct|Data)\.(new|define)\b")),
    ("method_missing", re.compile(r"\bmethod_missing\b")),
]

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def checkout_roots():
    out = subprocess.run([BIN, "--status", "--json"], capture_output=True).stdout
    try:
        return sorted(
            (c["repo"] for c in json.loads(out)["checkouts"]), key=len, reverse=True
        )
    except (json.JSONDecodeError, KeyError):
        return []


def under(root, path):
    return path.startswith(root + "/")


def ask(site):
    spec = f"{site['file']}:{site['line']}:{site['col']}"
    done = subprocess.run([BIN, "--def", spec, "--json"], capture_output=True)
    try:
        return json.loads(done.stdout)
    except json.JSONDecodeError:
        return None


def found_truth(answer, site):
    """Did trekr name the true definition, resolved or as a candidate?"""
    places = list(answer.get("sites") or [])
    places += [c.get("site", {}) for c in answer.get("candidates") or []]
    for place in places:
        path = place.get("path") or ""
        if os.path.realpath(path) != os.path.realpath(site["def_file"]):
            continue
        if abs(int(place.get("line", -99)) - int(site["def_line"])) <= 1:
            return True
    return False


def extracted_names(path, cache={}):
    """Every name trekr extracts from a file, with its line."""
    if path not in cache:
        out = subprocess.run(
            [BIN, "--symbols", path, "--json"], capture_output=True
        ).stdout
        try:
            cache[path] = [(r["name"], r["line"]) for r in json.loads(out)]
        except (json.JSONDecodeError, TypeError):
            cache[path] = []
    return cache[path]


def source_line(path, line, cache={}):
    if path not in cache:
        try:
            cache[path] = open(path, encoding="utf-8", errors="replace").read().split("\n")
        except OSError:
            cache[path] = []
    lines = cache[path]
    return lines[line - 1] if 0 < line <= len(lines) else ""


def why_not_reached(answer):
    """Trekr has the definition. What stopped the lookup getting to it?

    The interesting split, because each half wants different work: an untyped
    receiver is the ladder's limit, while a typed receiver whose chain simply
    does not contain the true owner is an ancestry or extraction bug.
    """
    receiver = answer.get("receiver") or "?"
    typed = answer.get("receiver_type")
    truncated = answer.get("unresolved_ancestors") or []
    if not typed:
        return f"receiver never typed — shape `{receiver}`"
    if truncated:
        return "receiver typed, but its ancestor chain is truncated"
    return "receiver typed, chain complete — the true owner is not in it"


def classify(site, roots, answer):
    definition = site["def_file"]
    if "/<core>" in definition or definition.endswith("core.rb"):
        return "core-stub", ""
    if not any(under(root, definition) for root in roots):
        # Which family of unindexed thing is it?
        for marker, label in (
            ("/gems/", "a gem outside every indexed checkout"),
            ("/ruby/", "Ruby's own stdlib"),
        ):
            if marker in definition:
                return "unindexed-source", label
        return "unindexed-source", "elsewhere"

    names = extracted_names(definition)
    near = [n for n, line in names if n == site["method"] and abs(line - site["def_line"]) <= 2]
    if near:
        return "not-reached", why_not_reached(answer)
    if any(n == site["method"] for n, _ in names):
        return "not-reached", "extracted, but at another line"

    text = source_line(definition, site["def_line"])
    for label, pattern in SHAPES:
        if pattern.search(text):
            return "not-extracted", label
    return "not-extracted", "no `def` and no shape we recognise"


def main(path):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    roots = checkout_roots()
    if not roots:
        sys.exit("no indexed checkouts; run --index first")
    gold = [json.loads(line) for line in open(path)]
    gold = [g for g in gold if g.get("def_file", "").endswith(".rb")]
    if SAMPLE:
        import random

        random.Random(12).shuffle(gold)
        gold = gold[:SAMPLE]

    buckets = collections.Counter()
    detail = collections.defaultdict(collections.Counter)
    examples = collections.defaultdict(list)
    absent = 0
    for i, site in enumerate(gold, 1):
        answer = ask(site)
        if answer is None or found_truth(answer, site):
            continue
        absent += 1
        bucket, why = classify(site, roots, answer)
        buckets[bucket] += 1
        detail[bucket][why] += 1
        if len(examples[(bucket, why)]) < 2:
            typed = (answer or {}).get("receiver_type")
            examples[(bucket, why)].append(
                f"{site['method']} on {typed or '?'} → {os.path.basename(site['def_file'])}"
            )
        if i % 50 == 0:
            print(f"  … {i}/{len(gold)}")

    print(f"\n{absent} of {len(gold)} gold sites: the truth is not among trekr's answers\n")
    width = max((len(b) for b in buckets), default=10)
    for bucket, n in buckets.most_common():
        print(f"  {bucket:<{width}}  {n:>4}  {100 * n / max(absent, 1):>5.1f}%")
        for why, m in detail[bucket].most_common():
            if why:
                sample = "; ".join(examples[(bucket, why)])
                print(f"      {m:>3}  {why}   e.g. {sample}")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/trekr-gold.ndjson")
