#!/usr/bin/env python3
"""How often does an edit actually change a file's *symbol surface*?

    script/churn.py ~/code/lib/ruby/rails ~/code/lib/ruby/discourse

The question behind edit-churn defence: today any changed blob invalidates the
checkout's assembled tree. If most edits touch method *bodies* rather than the
definition surface, the tree could stand and only line metadata would need
patching. This measures the "most" — before anything is built on it.

**What counts as the surface.** What the tree layer assembles from: each
definition's kind, lexical nesting, name, singleton flag, visibility, arity,
`via`, `target` and `sig_returns` — and the ancestry edges. Deliberately *not*
line or column: a definition that only moved is the case this exists to catch.

**How ancestry is compared.** `--symbols` reports definitions, not ancestry, so
`include`/`extend`/`prepend`/`class X < Y` lines are compared textually between
the two blob versions. They are single-line constructs, so this is tight, but
it is a proxy and the report says so.

Every corpus must be a git checkout (DEC-001). Blobs are read with
`git cat-file` and written to a scratch file, because `--symbols` reads a path.
"""

import collections, json, os, re, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target/release/trekr")
COMMITS = int(os.environ.get("COMMITS", "500"))

# Single-line constructs that move a class or module in the ancestor chain.
ANCESTRY = re.compile(
    rb"^\s*(include|extend|prepend)\s+[A-Z:][\w:]*|^\s*class\s+[\w:]+\s*<\s*[\w:]+",
    re.MULTILINE,
)

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def git(repo, args):
    return subprocess.run(
        ["git", "-C", repo] + args, capture_output=True, check=False
    ).stdout


def surface(scratch, blob):
    """The tree-relevant shape of one blob, or None if it would not parse."""
    with open(scratch, "wb") as handle:
        handle.write(blob)
    out = subprocess.run(
        [BIN, "--symbols", scratch, "--json"], capture_output=True, check=False
    ).stdout
    try:
        rows = json.loads(out)
    except json.JSONDecodeError:
        return None, None
    shape = [
        (
            r["kind"],
            tuple(r.get("nesting", [])),
            r["name"],
            r["singleton"],
            r["visibility"],
            len(r.get("params", [])),
            r.get("via"),
            r.get("target"),
            r.get("sig_returns"),
        )
        for r in rows
    ]
    positions = [(r["line"], r["col"]) for r in rows]
    return shape, positions


def pairs(repo, limit):
    """(old_oid, new_oid) for every modified .rb file in the last `limit` commits."""
    log = git(
        repo,
        [
            "log",
            f"-{limit}",
            "--format=%H",
            "--diff-filter=M",
            "--raw",
            "--no-renames",
            "--",
            "*.rb",
        ],
    ).decode("utf-8", "replace")
    for line in log.splitlines():
        # `:100644 100644 <old> <new> M\tpath`
        if not line.startswith(":"):
            continue
        fields = line.split()
        if len(fields) < 5:
            continue
        old, new = fields[2], fields[3]
        if old.startswith("0000") or new.startswith("0000"):
            continue
        yield old, new


def measure(repo):
    name = os.path.basename(os.path.realpath(repo))
    counts = collections.Counter()
    scratch = tempfile.NamedTemporaryFile(suffix=".rb", delete=False).name
    seen = set()
    for old, new in pairs(repo, COMMITS):
        if (old, new) in seen:
            continue
        seen.add((old, new))
        old_bytes, new_bytes = git(repo, ["cat-file", "blob", old]), git(
            repo, ["cat-file", "blob", new]
        )
        old_shape, old_pos = surface(scratch, old_bytes)
        new_shape, new_pos = surface(scratch, new_bytes)
        if old_shape is None or new_shape is None:
            counts["unparsed"] += 1
            continue
        counts["blobs"] += 1
        ancestry_same = sorted(ANCESTRY.findall(old_bytes)) == sorted(
            ANCESTRY.findall(new_bytes)
        )
        if old_shape == new_shape and ancestry_same:
            counts["surface_same"] += 1
            if old_pos == new_pos:
                counts["positions_same_too"] += 1
        elif old_shape == new_shape:
            counts["ancestry_only"] += 1
        else:
            counts["surface_changed"] += 1
    os.unlink(scratch)
    return name, counts


def main(repos):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    print(f"symbol-surface churn over the last {COMMITS} commits\n")
    header = f"{'corpus':<14}{'blobs':>8}{'surface same':>15}{'+ no move':>12}{'changed':>10}"
    print(header)
    print("-" * len(header))
    totals = collections.Counter()
    for repo in repos:
        repo = os.path.expanduser(repo)
        if not os.path.isdir(os.path.join(repo, ".git")):
            print(f"{os.path.basename(repo):<14}  not a git checkout — skipped")
            continue
        name, counts = measure(repo)
        totals.update(counts)
        blobs = counts["blobs"] or 1
        same = counts["surface_same"]
        print(
            f"{name:<14}{counts['blobs']:>8}"
            f"{same:>9} {100 * same / blobs:>4.0f}%"
            f"{counts['positions_same_too']:>7} {100 * counts['positions_same_too'] / blobs:>3.0f}%"
            f"{counts['surface_changed'] + counts['ancestry_only']:>10}"
        )
    blobs = totals["blobs"] or 1
    print(
        f"\nall{'':<11}{totals['blobs']:>8}"
        f"{totals['surface_same']:>9} {100 * totals['surface_same'] / blobs:>4.0f}%"
        f"{totals['positions_same_too']:>7} {100 * totals['positions_same_too'] / blobs:>3.0f}%"
        f"{totals['surface_changed'] + totals['ancestry_only']:>10}"
    )
    print(
        f"\nof the changed: {totals['ancestry_only']} changed ancestry only, "
        f"{totals['surface_changed']} changed a definition. "
        f"{totals['unparsed']} blob(s) would not parse and were dropped."
    )


if __name__ == "__main__":
    main(sys.argv[1:] or ["~/code/lib/ruby/rails"])
