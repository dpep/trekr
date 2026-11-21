#!/usr/bin/env python3
"""Score `--dead` against Spoom and sorbet-deadcode by git history (BASELINE session 36).

Each tool is run on a checkout at an OLD commit; a method candidate counts as
deleted when today's checkout (NEW) no longer defines it. The control is the
same question asked of every method in the scope, candidate or not: a tool
earns a lift only by beating that base rate.

    script/dead_compare.py --old <worktree at old commit> --new <today's checkout> \\
        --out <dir> [--scope app/models --scope app/services]

Tools run once each and cache their output in --out (delete a file to re-run):

    trekr.json    TREKR_BIN (default target/release/trekr) --dead SCOPE --json,
                  with TREKR_DB=<out>/trekr.db
    spoom.ndjson  script/spoom_dead.rb on the whole OLD checkout
    sd.json       sorbet-deadcode SCOPE --reference-root . --kind all --classify --format json
    defs-old.ndjson / defs-new.ndjson   script/dead_defs.rb, the inventories

COMPARE_RUBY names a Ruby >= 3.4 (sorbet-deadcode needs it), COMPARE_GEM_HOME
a gem home holding spoom, SORBET_DEADCODE the sorbet-deadcode checkout.

Two ground truths, both reported. "owner": the same owner, name and side are
still defined today, so a method moved to a concern counts as deleted.
"name" (session 36's): a method of that name is defined anywhere today.
Candidates are joined to the inventory by path:line, so no tool's naming
convention matters. Only methods are scored; Spoom and sorbet-deadcode's
other kinds are counted in the overlap section.
"""
import argparse
import json
import os
import subprocess
import sys
from collections import Counter, defaultdict

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)


def run_once(path, argv, cwd, env=None):
    if os.path.exists(path) and os.path.getsize(path) > 0:
        return
    print(f"running: {' '.join(argv)}", file=sys.stderr)
    with open(path, "w") as out, open(path + ".err", "w") as err:
        rc = subprocess.run(argv, cwd=cwd, stdout=out, stderr=err, env={**os.environ, **(env or {})}).returncode
    if rc != 0:
        print(f"  exit {rc}; see {path}.err", file=sys.stderr)


def ndjson(path):
    with open(path) as f:
        return [json.loads(line) for line in f if line.strip()]


def deleted_cell(k, n):
    """k of n, the rate and its Wilson 95 % interval: most sets here are small
    and the base rate low, so the interval is the honest part of the number."""
    if not n:
        return "n/a"
    z, p = 1.96, k / n
    centre = (p + z * z / (2 * n)) / (1 + z * z / n)
    half = z * ((p * (1 - p) / n + z * z / (4 * n * n)) ** 0.5) / (1 + z * z / n)
    lo, hi = max(0, centre - half), min(1, centre + half)
    return f"{k} ({100 * p:.2g} %, {100 * lo:.2g}–{100 * hi:.2g})"


def lift(rate, base):
    return f"{rate / base:.2g}×" if base and rate is not None else "n/a"


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--old", required=True)
    ap.add_argument("--new", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--scope", action="append")
    a = ap.parse_args()
    scope = a.scope or ["app/models", "app/services"]
    old, new, out = (os.path.abspath(p) for p in (a.old, a.new, a.out))
    os.makedirs(out, exist_ok=True)
    ruby = os.environ.get("COMPARE_RUBY", "ruby")
    gem_env = {}
    if os.environ.get("COMPARE_GEM_HOME"):
        gem_env = {"GEM_HOME": os.environ["COMPARE_GEM_HOME"], "GEM_PATH": os.environ["COMPARE_GEM_HOME"]}
    sd_repo = os.environ.get("SORBET_DEADCODE", os.path.expanduser("~/code/lib/ruby/sorbet-deadcode"))
    trekr = os.environ.get("TREKR_BIN", os.path.join(REPO, "target/release/trekr"))

    run_once(f"{out}/defs-old.ndjson", [ruby, f"{HERE}/dead_defs.rb", old, *scope], old)
    run_once(f"{out}/defs-new.ndjson", [ruby, f"{HERE}/dead_defs.rb", new], new)
    run_once(f"{out}/trekr.json", [trekr, "--dead", *scope, "--json"], old, {"TREKR_DB": f"{out}/trekr.db"})
    run_once(f"{out}/spoom.ndjson", [ruby, f"{HERE}/spoom_dead.rb", old], old, gem_env)
    run_once(f"{out}/sd.json", [ruby, f"-I{sd_repo}/lib", f"{sd_repo}/exe/sorbet-deadcode", *scope,
                                "--reference-root", ".", "--kind", "all", "--classify", "--format", "json",
                                "--max-age", "0"], old, gem_env)

    inventory = {(d["path"], d["line"]): d for d in ndjson(f"{out}/defs-old.ndjson")}
    today = ndjson(f"{out}/defs-new.ndjson")
    alive_owner = {(d["owner"], d["name"], d["singleton"]) for d in today}
    alive_name = {d["name"] for d in today}

    def deleted(key, truth):
        d = inventory[key]
        if truth == "owner":
            return (d["owner"], d["name"], d["singleton"]) not in alive_owner
        return d["name"] not in alive_name

    in_scope = lambda p: any(p == s or p.startswith(s.rstrip("/") + "/") for s in scope)

    # Each tool's method candidates as {(path, line): label}.
    groups = defaultdict(set)
    unjoined = Counter()
    trekr_rows = json.load(open(f"{out}/trekr.json"))["candidates"]
    for r in trekr_rows:
        if r["kind"] != "method":
            continue
        key = (r["path"], r["line"])
        if key not in inventory:
            unjoined["trekr"] += 1
            continue
        groups["trekr: all tiers"].add(key)
        groups[f"trekr: {r['tier']}"].add(key)
        if r["tier"] == "unreferenced":
            groups[f"trekr: unreferenced, {r['confidence']}"].add(key)
    spoom_rows = [r for r in ndjson(f"{out}/spoom.ndjson") if in_scope(r["path"])]
    for r in spoom_rows:
        if r["kind"] != "method":
            continue
        key = (r["path"], r["line"])
        if key not in inventory:
            unjoined["spoom"] += 1
            continue
        groups["spoom: all"].add(key)
    sd_rows = []
    try:
        sd_rows = json.load(open(f"{out}/sd.json"))
    except (OSError, ValueError):
        print("sd.json missing or unreadable", file=sys.stderr)
    for r in sd_rows:
        if r["kind"] != "method":
            continue
        path, line = r["location"].rsplit(":", 1)
        key = (path, int(line))
        if key not in inventory:
            unjoined["sorbet-deadcode"] += 1
            continue
        groups["sorbet-deadcode: all"].add(key)
        groups[f"sorbet-deadcode: {r['action']}"].add(key)

    base = set(inventory)
    t, s, d = groups["trekr: unreferenced"], groups["spoom: all"], groups["sorbet-deadcode: all"]
    groups["spoom and sorbet-deadcode"] = s & d
    groups["trekr unreferenced and spoom"] = t & s
    groups["trekr unreferenced and sorbet-deadcode"] = t & d
    groups["all three"] = t & s & d

    print(f"old: {old}\nnew: {new}\nscope: {' '.join(scope)}\n")
    print("| set | methods | deleted, by owner (rate, 95 % CI) | lift | deleted, by name | lift |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    base_rate = {tr: sum(deleted(k, tr) for k in base) / len(base) for tr in ("owner", "name")}

    def row(label, keys):
        cells = [label, str(len(keys))]
        for tr in ("owner", "name"):
            n = sum(deleted(k, tr) for k in keys)
            cells += [deleted_cell(n, len(keys)), lift(n / len(keys) if keys else None, base_rate[tr])]
        print("| " + " | ".join(cells) + " |")

    row("**base rate: every method in scope**", base)
    for label in sorted(groups, key=lambda g: (g.split(":")[0], -len(groups[g]))):
        row(label, groups[label])
    if unjoined:
        print(f"\nnot in the inventory (no `def` at that path:line): {dict(unjoined)}")

    # Overlap of the three candidate sets, every kind, keyed by path:line.
    def keyset(rows, path_line):
        return {path_line(r): r["kind"] for r in rows}
    sets = {
        "trekr": {(r["path"], r["line"]): r["kind"] for r in trekr_rows if r["tier"] == "unreferenced"},
        "spoom": keyset(spoom_rows, lambda r: (r["path"], r["line"])),
        "sorbet-deadcode": keyset(sd_rows, lambda r: (r["location"].rsplit(":", 1)[0], int(r["location"].rsplit(":", 1)[1]))),
    }
    print("\nCandidates by kind (trekr: `unreferenced` only):")
    for name, ks in sets.items():
        print(f"  {name}: {len(ks)} {dict(Counter(ks.values()))}")
    tk, sk, dk = (set(v) for v in sets.values())
    print(f"  only trekr {len(tk - sk - dk)}, only spoom {len(sk - tk - dk)}, only sorbet-deadcode {len(dk - tk - sk)}, "
          f"trekr+spoom {len((tk & sk) - dk)}, trekr+sd {len((tk & dk) - sk)}, spoom+sd {len((sk & dk) - tk)}, all three {len(tk & sk & dk)}")
    trekr_any = {(r["path"], r["line"]): r["tier"] for r in trekr_rows}
    for name, ks in (("spoom", sk), ("sorbet-deadcode", dk)):
        tiers = Counter(trekr_any.get(k, "not reported") for k in ks)
        print(f"  {name}'s candidates, by trekr tier: {dict(tiers.most_common())}")


if __name__ == "__main__":
    main()
