#!/usr/bin/env python3
"""What are the receivers trekr declines on, when it already has the answer?

    script/declined.py /tmp/trekr-gold-discourse.ndjson

`script/absent.py` asks why the truth is *missing*. This asks the opposite
question, which on real app code is the bigger one: discourse's largest single
bucket is **residue with the truth offered** — 40.5 % of app sites, where the
ladder declines to commit and the ranker then puts the right answer first 87 %
of the time. That is a population where the answer is already in hand and only
the confidence to promote it is missing, so the shape of those receivers is the
shape of the next rung.

Two things are reported, and the second is the one a proposal is built from.

**The distribution** — every declined app site by receiver shape crossed with
what the receiver expression actually *is*. `receiver: "other"` covers a chain,
a literal, a block parameter and a bare method call with the same word; the
source line tells them apart, and they want different work.

**The promotion ceiling per slice** — if a rung promoted this slice's top
candidate outright, how many sites become `correct` and how many become
`confidently wrong`? The denominator is every declined site in the slice, not
just the ones where the truth was offered: a rung cannot decline the sites it
would get wrong. This is the number that decides whether a slice is worth
building, before anything is built.
"""

import collections, json, os, random, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("TREKR_BIN") or os.path.join(ROOT, "target/release/trekr")
SAMPLE = int(os.environ.get("SAMPLE", "0"))
SEED = int(os.environ.get("SEED", "12"))
WORKERS = int(os.environ.get("WORKERS", "6"))
CONTEXT = os.environ.get("CONTEXT")
SCOPE = os.environ.get("SCOPE", "app")

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def ask(site):
    spec = f"{site['file']}:{site['line']}:{site['col']}"
    argv = [BIN, "--def", spec, "--json"]
    if CONTEXT:
        argv += ["--context", CONTEXT]
    done = subprocess.run(argv, capture_output=True, check=False)
    try:
        return json.loads(done.stdout)
    except json.JSONDecodeError:
        return None


def source_line(path, line, cache={}):
    if path not in cache:
        try:
            cache[path] = open(path, encoding="utf-8", errors="replace").read().split("\n")
        except OSError:
            cache[path] = []
    lines = cache[path]
    return lines[line - 1] if 0 < line <= len(lines) else ""


CLOSERS = {")": "(", "]": "[", "}": "{"}


def receiver_text(path, line, col):
    """The receiver expression written to the left of the method name.

    The engine knows this exactly and does not report it — only the shape
    (`other`) survives into the answer. Recovering it from the source is
    approximate at the edges (a receiver split across lines reads as empty),
    and those land in their own bucket rather than being guessed at.
    """
    text = source_line(path, line)
    head = text[: col - 1] if col >= 1 else ""
    stripped = head.rstrip()
    # `foo&.bar` is `foo.bar` for this purpose.
    if stripped.endswith("&."):
        stripped = stripped[:-2]
    elif stripped.endswith("."):
        stripped = stripped[:-1]
    else:
        return ""  # no explicit receiver on this line
    stripped = stripped.rstrip()
    if not stripped:
        return "<continued>"  # `foo\n  .bar` — the receiver is on another line

    # Walk left over one expression, balancing brackets so that a call's
    # arguments come with it.
    depth = []
    i = len(stripped)
    while i > 0:
        ch = stripped[i - 1]
        if ch in CLOSERS:
            depth.append(CLOSERS[ch])
            i -= 1
            continue
        if depth:
            if ch == depth[-1]:
                depth.pop()
            i -= 1
            continue
        if ch.isalnum() or ch in "_@$?!.:&\"'":
            i -= 1
            continue
        break
    return stripped[i:].strip()


BARE_NAME = re.compile(r"\A[a-z_]\w*[?!]?\Z")
CONST_NAME = re.compile(r"\A[A-Z]\w*(::[A-Z]\w*)*\Z")


def expression_kind(expr):
    """What the receiver expression is, syntactically."""
    if not expr:
        return "implicit — no receiver written"
    if expr == "<continued>":
        return "receiver on a previous line"
    if expr == "self":
        return "self"
    if expr.startswith("@@"):
        return "class variable"
    if expr.startswith("@"):
        return "instance variable" if "." not in expr else "chained call"
    if expr.startswith("$"):
        return "global variable"
    if "." in expr or "&." in expr:
        return "chained call"
    if expr.endswith(")"):
        return "call with arguments"
    if expr.endswith("]"):
        return "element reference"
    if CONST_NAME.match(expr):
        return "constant"
    if BARE_NAME.match(expr):
        return "bare name — a local, a parameter or a call on self"
    if expr[:1] in "\"'":
        return "string literal"
    if expr[:1].isdigit():
        return "numeric literal"
    return "other expression"


SINGLETON = re.compile(r"\A#<Class:")


def owner_kind(owner):
    """What kind of thing owns the method Ruby actually ran."""
    if not owner:
        return "unknown"
    if SINGLETON.match(owner):
        return "a singleton class"
    if owner.endswith("ClassMethods"):
        return "a concern's ClassMethods"
    if owner in ("Kernel", "Object", "BasicObject", "Module", "Class"):
        return f"Ruby's {owner}"
    return "an ordinary class or module"


def where(path, app_root):
    if "/gems/" in path:
        return "gem"
    if path.startswith(app_root):
        return "app"
    return "elsewhere"


def hits(place, site):
    path = place.get("path") or ""
    if os.path.realpath(path) != os.path.realpath(site["def_file"]):
        return False
    return abs(int(place.get("line", -99)) - int(site["def_line"])) <= 1


def truth_rank(answer, site):
    """1-based position of the truth among the candidates, or None."""
    for i, candidate in enumerate(answer.get("candidates") or [], 1):
        if hits(candidate.get("site", {}), site):
            return i
    return None


def checkout_root(files):
    if not files:
        return "/dev/null"
    here = os.path.commonpath(files)
    while here != "/":
        if os.path.exists(os.path.join(here, ".git")):
            return here
        here = os.path.dirname(here)
    return os.path.commonpath(files)


def classify(site, answer, app_root):
    expr = receiver_text(site["file"], site["line"], site["col"])
    # The candidate count rides on the end of the reason string, which would
    # otherwise split one reason into thirty.
    reason = (answer.get("reason") or "").split("; showing")[0]
    return {
        "shape": answer.get("receiver") or "?",
        "expression": expression_kind(expr),
        "typed": bool(answer.get("receiver_type")),
        "type": answer.get("receiver_type") or "",
        "reason": reason,
        "method": site["method"],
        "at": f"{site['file'].split('/app/')[-1]}:{site['line']}",
        "truth": f"{os.path.basename(site['def_file'])}:{site['def_line']}",
        "owner_name": site.get("owner", ""),
        "owner": owner_kind(site.get("owner")),
        "truth_in": where(site["def_file"], app_root),
        "rank": truth_rank(answer, site),
        "candidates": len(answer.get("candidates") or []),
        # A "nothing defines this name" is only as strong as this list is
        # short: an unresolved ancestor is a coverage gap wearing a lookup
        # gap's clothes.
        "truncated": bool(answer.get("unresolved_ancestors")),
        # Ruby dispatched to a class-level method, so the receiver was a class
        # — which for an implicit receiver means a class-body macro call.
        "class_level": owner_kind(site.get("owner"))
        in ("a singleton class", "a concern's ClassMethods"),
    }


def table(title, rows, key, total):
    print(f"\n{title}")
    tally = collections.Counter(key(r) for r in rows)
    width = max((len(k) for k in tally), default=10)
    for name, n in tally.most_common():
        print(f"  {name:<{width}}  {n:>4}  {100 * n / max(total, 1):>5.1f}%")


def ceiling(title, rows, key):
    """If a rung promoted the top candidate for this slice, what happens?

    Every declined site in the slice is in the denominator: a rung that fires
    on a shape fires on all of it, including the sites where the truth was
    never offered. Those become confident wrong answers, which is the cost the
    bar exists to price.
    """
    print(f"\n{title}")
    groups = collections.defaultdict(list)
    for row in rows:
        groups[key(row)].append(row)
    width = max((len(k) for k in groups), default=10)
    print(f"  {'slice':<{width}}  {'sites':>5}  {'→correct':>9}  {'→wrong':>7}  {'precision':>9}")
    for name, group in sorted(groups.items(), key=lambda kv: -len(kv[1])):
        good = sum(1 for r in group if r["rank"] == 1)
        bad = len(group) - good
        print(
            f"  {name:<{width}}  {len(group):>5}  {good:>9}  {bad:>7}"
            f"  {100 * good / len(group):>8.1f}%"
        )


def main(path):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    gold = [json.loads(line) for line in open(path)]
    gold = [g for g in gold if g.get("def_file", "").endswith(".rb")]
    app_root = os.environ.get("APP_ROOT") or checkout_root(
        [g["file"] for g in gold if g["scope"] == "app"]
    )
    sites = [g for g in gold if g["scope"] == SCOPE]
    if SAMPLE and len(sites) > SAMPLE:
        random.Random(SEED).shuffle(sites)
        sites = sites[:SAMPLE]

    done = [0]

    def one(site):
        answer = ask(site)
        done[0] += 1
        if done[0] % 250 == 0:
            print(f"  … {done[0]}/{len(sites)}")
        if not isinstance(answer, dict):
            return None
        if answer.get("reason") == "no name at this position":
            return None
        if answer.get("name") and answer["name"] != site["method"]:
            return None  # a gold-column fault, not an engine outcome
        if answer.get("status") == "resolved" or answer.get("sites"):
            return None  # trekr committed; this script is about the declines
        return classify(site, answer, app_root)

    with ThreadPoolExecutor(max_workers=WORKERS) as pool:
        rows = [r for r in pool.map(one, sites) if r]

    total = len(rows)
    offered = sum(1 for r in rows if r["rank"])
    first = sum(1 for r in rows if r["rank"] == 1)
    print(f"\n{total} declined {SCOPE} sites of {len(sites)} scored")
    print(f"  truth offered as a candidate   {offered:>4}  {100 * offered / max(total, 1):>5.1f}%")
    print(f"  truth ranked first             {first:>4}  {100 * first / max(total, 1):>5.1f}%")

    table("by receiver shape, as the engine reports it:", rows, lambda r: r["shape"], total)
    table("by what the receiver expression is:", rows, lambda r: r["expression"], total)
    table("by why the engine declined:", rows, lambda r: r["reason"] or "(none given)", total)
    table("by what owns the method Ruby ran:", rows, lambda r: r["owner"], total)
    table("by where the truth lives:", rows, lambda r: r["truth_in"], total)

    ceiling(
        "promotion ceiling, by receiver expression:",
        rows,
        lambda r: r["expression"],
    )
    ceiling(
        "promotion ceiling, by shape × typed:",
        rows,
        lambda r: f"{r['shape']}, {'typed' if r['typed'] else 'untyped'}",
    )
    # The split that decides what a rung would be *for*. An implicit receiver
    # whose method Ruby found on a class is a class-body macro call, and wants
    # ancestry work; one that dispatched to an instance method is an ordinary
    # call in a method body, and wants something else entirely.
    ceiling(
        "promotion ceiling, receiver expression × what owns the truth:",
        rows,
        lambda r: f"{r['expression'][:34]} → {r['owner']}",
    )

    if os.environ.get("ROWS"):
        with open(os.environ["ROWS"], "w") as out:
            for row in rows:
                out.write(json.dumps(row) + "\n")
        print(f"\nwrote {len(rows)} classified rows to {os.environ['ROWS']}")

    if os.environ.get("EXAMPLES"):
        n = int(os.environ["EXAMPLES"])
        groups = collections.defaultdict(list)
        for row in rows:
            groups[row["expression"]].append(row)
        for name, group in sorted(groups.items(), key=lambda kv: -len(kv[1])):
            print(f"\n{name} — {len(group)} sites, e.g.")
            for row in group[:n]:
                mark = "#1" if row["rank"] == 1 else (f"#{row['rank']}" if row["rank"] else "--")
                print(
                    f"  {mark:>3}  {row['at']:<44} {row['method']:<24}"
                    f" recv={row['type'] or '?':<28} owner={row['owner_name']:<40}"
                    f" → {row['truth']}"
                )


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "/tmp/trekr-gold.ndjson")
