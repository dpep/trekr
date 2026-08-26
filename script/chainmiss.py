#!/usr/bin/env python3
"""Why is the true owner not in the receiver's ancestor chain?

    script/chainmiss.py /tmp/trekr-declined-rows.ndjson

The largest thing left in discourse's residue is not a typing gap: the receiver
*is* resolved, and the method Ruby ran belongs to something the chain we built
does not contain. `script/declined.py` finds those sites; this asks what edge is
missing, because the answers want different work and only some of them are work
at all.

The first cut is the one that decides everything after it: **does the owner
exist in the index?** A missing owner is a coverage gap — the file was never
read, or nothing we extract declares that name. A *present* owner with no path
to it is a missing edge, and only that half is an ancestry problem.

Everything downstream of that is a guess until it is checked by hand, which is
why `--sample` prints sites per bucket: a bucket nobody has read one example of
is a number, not a finding.
"""

import argparse, collections, json, os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("TREKR_BIN") or os.path.join(ROOT, "target/release/trekr")
CONTEXT = os.environ.get("CONTEXT", "/Users/dpepper/code/lib/ruby/discourse")

SINGLETON = re.compile(r"\A#<Class:([^ (>]+)")

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def ancestors(name, cache={}):
    """trekr's own chain for a constant, or None when it knows no such name."""
    if name not in cache:
        done = subprocess.run(
            [BIN, "--ancestors", name, "--json", "--context", CONTEXT],
            capture_output=True,
            cwd=CONTEXT,
        )
        try:
            value = json.loads(done.stdout)
        except json.JSONDecodeError:
            value = {}
        # An unknown constant comes back as a residue object with no
        # `ancestors` key at all — distinct from a known one whose chain is
        # legitimately short. Collapsing the two with `or []` would have made
        # the "owner absent" bucket unreachable, which is the bucket the whole
        # classification turns on.
        chain = value.get("ancestors")
        cache[name] = chain if isinstance(chain, list) else None
    return cache[name]


def owner_shape(owner):
    """What kind of thing Ruby says owns the method."""
    if SINGLETON.match(owner):
        return "singleton"
    if owner.endswith("ClassMethods"):
        return "class-methods"
    return "plain"


def bare_owner(owner):
    """`#<Class:Foo(id: integer, …)>` -> `Foo`; otherwise the name as written."""
    found = SINGLETON.match(owner)
    return found.group(1) if found else owner


def classify(row):
    owner = row.get("owner_name") or ""
    receiver = row.get("type") or ""
    if not owner or not receiver:
        return "no receiver or owner recorded", None

    target = bare_owner(owner)
    known = ancestors(target)
    shape = owner_shape(owner)

    # The decisive split: is the owner even in the index?
    if known is None:
        return f"owner absent from the index ({shape})", target

    chain = ancestors(receiver)
    if chain is None:
        return "receiver's own type absent from the index", receiver
    if target in chain:
        # The owner *is* reachable — so the miss is not the chain at all. Most
        # likely the method is not extracted from that owner.
        return f"owner is in the chain; the method is not on it ({shape})", target
    return f"owner known, no edge to it ({shape})", target


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("rows")
    parser.add_argument("--sample", type=int, default=0, help="print N sites per bucket")
    parser.add_argument("--workers", type=int, default=6)
    args = parser.parse_args()

    rows = [json.loads(line) for line in open(args.rows)]
    # The population: receiver typed, chain not truncated, truth never named.
    rows = [
        r
        for r in rows
        if r.get("typed") and not r.get("truncated") and not r.get("rank")
    ]
    print(f"{len(rows)} sites: receiver typed, chain complete, truth never named\n")

    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        verdicts = list(pool.map(classify, rows))

    buckets = collections.Counter(v for v, _ in verdicts)
    total = len(rows) or 1
    width = max((len(b) for b in buckets), default=10)
    # Not "truth at #1" — every row here is one the truth never reached, so that
    # column would read zero by construction. What varies is whether the caller
    # got a list at all.
    print(f"  {'why the chain misses it':<{width}} {'sites':>6} {'share':>7} {'no list':>8}")
    for name, count in buckets.most_common():
        group = [r for r, (v, _) in zip(rows, verdicts) if v == name]
        blank = sum(1 for r in group if not r.get("candidates"))
        print(f"  {name:<{width}} {count:>6} {100 * count / total:>6.1f}% {blank:>8}")

    if args.sample:
        for name, _ in buckets.most_common():
            group = [
                (r, t) for r, (v, t) in zip(rows, verdicts) if v == name
            ][: args.sample]
            print(f"\n{name} — e.g.")
            for row, target in group:
                print(
                    f"  {row['at']:<46} {row['method']:<24}"
                    f" recv={row.get('type', '?'):<30} owner={target}"
                )


if __name__ == "__main__":
    main()
