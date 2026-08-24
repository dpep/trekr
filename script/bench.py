#!/usr/bin/env python3
"""Reproduce the numbers in docs/ARCHITECTURE.md.

    script/bench.py ~/code/lib/ruby/rails ~/code/lib/ruby/ruby

Uses a throwaway database, so it never touches the real index. Every corpus
must be a git checkout (DEC-001); a source drop with no `.git` is skipped
loudly rather than silently.

Cold time is measured once — the second run is by definition not cold. The
no-op and query timings are medians of five, which is the precision those
numbers are reported to.
"""

import json, os, shutil, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target/release/trekr")
DB = "/tmp/trekr-bench.db"
# Names spanning three orders of magnitude of match count. `new` is the one
# that took 90 s before the store learned to keep statistics.
QUERIES = ["find_each", "save", "each", "new"]


def trekr(args, cwd=None):
    return subprocess.run(
        [BIN, *args], capture_output=True, text=True, cwd=cwd,
        env=dict(os.environ, TREKR_DB=DB),
    )


def median_ms(args, cwd=None, n=5):
    times = []
    for _ in range(n):
        start = time.perf_counter()
        trekr(args, cwd)
        times.append(time.perf_counter() - start)
    return sorted(times)[n // 2] * 1000


def main(corpora):
    if not os.path.exists(BIN):
        sys.exit(f"build it first: cargo build --release ({BIN} missing)")
    for suffix in ("", "-wal", "-shm"):
        try:
            os.remove(DB + suffix)
        except FileNotFoundError:
            pass

    indexed = []
    for repo in corpora:
        repo = os.path.expanduser(repo)
        name = os.path.basename(repo.rstrip("/"))
        if not os.path.isdir(os.path.join(repo, ".git")):
            print(f"{name}: not a git checkout — skipped (DEC-001)")
            continue
        before = os.path.getsize(DB) / 1e6 if os.path.exists(DB) else 0

        start = time.perf_counter()
        run = trekr(["--index", repo, "--json"])
        cold = time.perf_counter() - start
        if run.returncode != 0:
            print(f"{name}: FAILED — {run.stderr.strip()}")
            continue

        counts = json.loads(run.stdout)["indexed"]
        noop = median_ms(["--index", repo])
        again = json.loads(trekr(["--index", repo, "--json"]).stdout)["indexed"]
        grew = os.path.getsize(DB) / 1e6 - before

        print(f"\n=== {name}")
        print(f"  {counts['files']:,} files, {counts['blobs']:,} blobs")
        print(f"  {counts['defs']:,} defs  {counts['refs']:,} const refs  "
              f"{counts['calls']:,} calls")
        print(f"  cold {cold:.1f} s   no-op {noop:.0f} ms "
              f"(parsed {again['parsed']})   +{grew:.0f} MB")
        indexed.append(repo)

    if not indexed:
        return
    # Query cost is measured on the first corpus, which is where the numbers in
    # ARCHITECTURE.md come from.
    where = indexed[0]
    print(f"\n=== --refs, in {os.path.basename(where)}")
    for name in QUERIES:
        rows = trekr(["--refs", name, "--json"], cwd=where).stdout
        count = len(json.loads(rows)) if rows.strip() else 0
        print(f"  {name:<12} {count:>7,} rows  "
              f"{median_ms(['--refs', name, '--json'], cwd=where):6.1f} ms")

    # A worktree that shares every blob should cost a scan and no parsing.
    clone = "/tmp/trekr-bench-worktree"
    shutil.rmtree(clone, ignore_errors=True)
    if subprocess.run(["git", "clone", "-q", "--shared", where, clone],
                      capture_output=True).returncode == 0:
        start = time.perf_counter()
        out = json.loads(trekr(["--index", clone, "--json"]).stdout)["indexed"]
        print(f"\n=== a second worktree of {os.path.basename(where)}")
        print(f"  {(time.perf_counter() - start) * 1000:.0f} ms, "
              f"{out['files']:,} files, parsed {out['parsed']}")
        shutil.rmtree(clone, ignore_errors=True)


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
