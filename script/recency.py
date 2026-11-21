#!/usr/bin/env python3
"""Does a `--dead` candidate's age predict its fate? (DEC-690)

    script/recency.py REPO OLD_COMMIT OLD_DEAD.ndjson HEAD_DEAD.ndjson [SCOPE...]

OLD_DEAD is `trekr --dead SCOPE --ndjson` run in a worktree of REPO at
OLD_COMMIT, HEAD_DEAD the same at REPO's HEAD. A method's age is that of
its `def` line, `git blame -w` at OLD_COMMIT (committer time). Its fate at
HEAD: `deleted` (no `def` of its name left in its file, renames followed),
`gained` (defined, but no longer a candidate: something now uses it), or
`still` a candidate. Every `def` in SCOPE is the base rate for `deleted`.
"""
import collections, json, math, os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

repo, old, old_dead, head_dead = sys.argv[1:5]
scope = sys.argv[5:] or ["app/models", "app/services"]
DEF = re.compile(r"^\s*def\s+(?:self\.)?([A-Za-z_][A-Za-z0-9_]*[?!=]?)")
BANDS = [(0, 30), (30, 90), (90, 365), (365, math.inf)]


def git(*a):
    return subprocess.run(["git", "-C", repo, *a], capture_output=True, text=True, check=True).stdout


def methods(path):
    for line in open(path):
        r = json.loads(line)
        if "answer" not in r and r["kind"] == "method":
            yield r


def blame(f):
    """Line -> committer time of the commit that wrote it."""
    when, line_of, cur = {}, {}, None
    for l in git("blame", "--porcelain", "-w", old, "--", f).split("\n"):
        m = re.match(r"^([0-9a-f]{40}) \d+ (\d+)", l)
        if m:
            cur = m.group(1)
            line_of[int(m.group(2))] = cur
        elif l.startswith("committer-time ") and cur:
            when[cur] = int(l.split()[1])
    return {n: when[c] for n, c in line_of.items()}


old_time = int(git("log", "-1", "--format=%ct", old))
files = [f for f in git("ls-tree", "-r", "--name-only", old, "--", *scope).split("\n") if f.endswith(".rb")]
with ThreadPoolExecutor(os.cpu_count()) as pool:
    blamed = dict(zip(files, pool.map(blame, files)))

renamed = {}
for l in git("diff", "--name-status", "-M", old, "HEAD", "--", *scope).split("\n"):
    p = l.split("\t")
    if p[0].startswith("R"):
        renamed[p[1]] = p[2]
    elif p[0] == "D":
        renamed[p[1]] = None


def defs(rev, f):
    try:
        return [(i, m.group(1)) for i, l in enumerate(git("show", f"{rev}:{f}").split("\n"), 1) if (m := DEF.match(l))]
    except subprocess.CalledProcessError:
        return []


at_head = {}
def survives(f, name):
    if f not in at_head:
        to = renamed.get(f, f)
        at_head[f] = {n for _, n in defs("HEAD", to)} if to else set()
    return name in at_head[f]


def band(f, line):
    age = (old_time - blamed[f][line]) / 86400
    return next(b for b in BANDS if b[0] <= age < b[1])


def label(b):
    return f"{b[0]}-{'' if b[1] == math.inf else b[1]}d"


def wilson(k, n, z=1.96):
    p, d = k / n, 1 + z * z / n
    c, h = (p + z * z / (2 * n)) / d, z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n)) / d
    return f"{100 * max(c - h, 0):.0f}-{100 * (c + h):.0f}%"


base = collections.defaultdict(collections.Counter)
for f in files:
    for line, name in defs(old, f):
        base[band(f, line)]["deleted" if not survives(f, name) else "kept"] += 1

still = {(renamed.get(r["path"], r["path"]), r["owner"], r["name"], r["singleton"]) for r in methods(head_dead)}
fates = collections.defaultdict(lambda: collections.defaultdict(collections.Counter))
for r in methods(old_dead):
    f = r["path"]
    if not survives(f, r["name"]):
        fate = "deleted"
    elif (renamed.get(f, f), r["owner"], r["name"], r["singleton"]) in still:
        fate = "still"
    else:
        fate = "gained"
    for group in ("all candidates", r["tier"]):
        fates[group][band(f, r["line"])][fate] += 1

print("## every def in scope (base rate for deleted)")
for b in BANDS:
    c = base[b]
    n = sum(c.values())
    if n:
        print(f"{label(b):>8} n={n:5d} deleted {c['deleted']:4d} ({100 * c['deleted'] / n:.1f}%, CI {wilson(c['deleted'], n)})")
for group, by_band in fates.items():
    print(f"\n## {group}")
    for b in BANDS:
        c = by_band[b]
        n = sum(c.values())
        if n:
            print(f"{label(b):>8} n={n:5d} deleted {c['deleted']:4d} ({100 * c['deleted'] / n:.1f}%)"
                  f"  gained {c['gained']:3d} ({100 * c['gained'] / n:.1f}%, CI {wilson(c['gained'], n)})  still {c['still']:4d}")
