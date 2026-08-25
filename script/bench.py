#!/usr/bin/env python3
"""Reproduce the numbers in docs/ARCHITECTURE.md.

    script/bench.py ~/code/lib/ruby/rails ~/code/lib/ruby/ruby

Uses a throwaway database, so it never touches the real index.

Every corpus must be a git checkout (DEC-001). Some useful ones are not:
`discourse` and `mastodon` are kept as source drops with no `.git`, so those
are staged into a scratch repo first. That staging is here rather than in a
README step because a number nobody can re-run is halfway to not having
happened.

**Conditions the numbers carry.** `discourse` and `mastodon` are gitless *and
partially bundled* — roughly 101 of discourse's 353 locked gems and 75 of
mastodon's 344 are present in the local gemdir, the rest are named and absent.
That is why method-call residue is reported split by whether the ancestor chain
the lookup walked was complete: blending a corpus whose dependencies are half
missing into one rate is the flattering-denominator trap.

Cold time is measured once — the second run is by definition not cold. The
no-op and query timings are medians of five, which is the precision those
numbers are reported to.
"""

import collections, hashlib, json, os, shutil, sqlite3, subprocess, sys, time

# Line-buffered even when redirected: a run takes minutes, and a progress
# report you cannot watch is not one.
try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.path.join(ROOT, "target/release/trekr")
DB = "/tmp/trekr-bench.db"
# Names spanning three orders of magnitude of match count. `new` is the one
# that took 90 s before the store learned to keep statistics.
QUERIES = ["find_each", "save", "each", "new"]
SAMPLE = 120

# Ruby core and common stdlib. Not indexed (PLAN Phase 3), so a residue naming
# one of these is a known gap rather than a resolution bug — and the difference
# is the only thing that makes the unresolved rate worth reporting.
CORE = {
    "Abbrev", "ARGV", "ArgumentError", "Array", "BasicObject", "Base64", "Benchmark",
    "BigDecimal", "Coverage", "ENV", "English", "Enumerator", "Fcntl", "Find", "IRB",
    "MonitorMixin", "OptionParser", "Prism", "RbConfig", "Readline", "Ripper",
    "RUBY_ENGINE", "RUBY_PLATFORM", "RUBY_VERSION", "StringScanner", "TSort", "Warning",
    "Comparable", "Complex", "CSV", "Data", "Date", "DateTime", "Delegator",
    "Digest", "Dir", "Encoding", "Enumerable", "ERB", "Errno", "Etc", "Exception",
    "Fiber", "File", "FileUtils", "Float", "Forwardable", "FrozenError", "GC",
    "Hash", "Integer", "IndexError", "IO", "IOError", "JSON", "Kernel", "KeyError",
    "LoadError", "Logger", "Marshal", "MatchData", "Math", "Method", "Minitest",
    "Monitor", "Mutex", "Net", "NameError", "NoMethodError", "NotImplementedError",
    "Numeric", "Object", "ObjectSpace", "Open3", "OpenSSL", "Pathname", "PP",
    "Proc", "Process", "Psych", "Queue", "Rack", "Ractor", "Random", "Range",
    "RangeError", "Rational", "Regexp", "RubyVM", "RuntimeError", "SecureRandom",
    "Set", "Shellwords", "Signal", "SimpleDelegator", "Singleton", "Socket",
    "StandardError", "String", "StringIO", "Struct", "Symbol", "SystemExit",
    "Tempfile", "Thread", "Time", "Timeout", "TypeError", "URI", "WeakRef",
    "YAML", "Zlib", "ZeroDivisionError",
}


# Deliberately not tempfile.gettempdir(): on macOS that is a per-user directory
# the OS prunes, and re-rsyncing 11k files to reproduce a number is a tax.
STAGING = "/tmp/trekr-bench-corpora"


def as_git_checkout(repo):
    """A path trekr can index: `repo` itself, or a staged copy of it.

    Content-addressing needs git (DEC-001), so a source drop is copied into a
    scratch repo and committed once. Staging is kept across runs — it costs a
    minute and the corpus does not change.
    """
    if os.path.isdir(os.path.join(repo, ".git")):
        return repo
    staged = os.path.join(STAGING, os.path.basename(repo.rstrip("/")))
    if os.path.isdir(os.path.join(staged, ".git")):
        return staged
    print(f"  staging {os.path.basename(repo)} into a scratch repo (no .git)...")
    os.makedirs(staged, exist_ok=True)
    subprocess.run(
        ["rsync", "-a", "--exclude", ".git", "--exclude", "node_modules",
         "--exclude", "tmp", repo.rstrip("/") + "/", staged + "/"],
        check=True,
    )
    for cmd in (["git", "init", "-q"], ["git", "add", "-A"],
                ["git", "-c", "user.email=b@e.st", "-c", "user.name=bench",
                 "commit", "-qm", "corpus"]):
        subprocess.run(cmd, cwd=staged, capture_output=True)
    return staged


def checkout_root(repo):
    """The path the store keys on — git's canonical one, not the one we typed."""
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"], cwd=repo, capture_output=True, text=True
    )
    return out.stdout.strip() or repo


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

    # Reproducibility: every number below came from this database and these
    # staged corpora. Printing them means a reviewer can re-run a single query
    # by copy-paste instead of reading this file.
    print(f"TREKR_DB={DB}   staging={STAGING}")
    indexed = []
    for repo in corpora:
        name = os.path.basename(repo.rstrip("/"))
        repo = os.path.expanduser(repo)
        if not os.path.isdir(repo):
            print(f"{name}: no such directory — skipped")
            continue
        repo = as_git_checkout(repo)
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

    # The tree layer is rebuilt from SQL on every invocation, with no
    # incremental machinery. `--refs` needs no tree and `--ancestors` needs a
    # whole one, so the gap between them is what a rebuild costs.
    print("\n=== tree rebuild (whole checkout, from facts)")
    for repo in indexed:
        name = os.path.basename(repo.rstrip("/"))
        baseline = median_ms(["--refs", "Widget", "--json"], repo)
        whole = median_ms(["--ancestors", "Object", "--json"], repo)
        print(f"  {name:<12} {whole - baseline:6.0f} ms "
              f"({whole:.0f} ms total, {baseline:.0f} ms of it process + query)")

    resolution_rate(indexed)
    call_resolution(indexed)

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


def stable_sample(rows, count):
    """The same `count` rows before and after a change to what gets indexed.

    `random.sample` draws by position, so a population that grows or shrinks by
    even one row re-draws the whole sample — and every before/after delta is
    then a comparison of two different populations. Ordering by a hash of each
    row's identity keeps the sample stable for the rows that still exist, which
    is what makes a delta mean anything.
    """
    ordered = sorted(rows, key=lambda r: hashlib.md5(
        f"{r[0]}:{r[1]}:{r[2]}".encode()).hexdigest())
    return ordered[:count]


def call_resolution(corpora):
    """What share of real call sites does the receiver ladder resolve?

    Session 1 measured the receiver *shapes* and predicted that implicit +
    self + const — 56% of call sites — need no inference at all. This is the
    check: does the ladder actually deliver that, or does something else eat
    it? Either answer is a finding.
    """
    print(f"\n=== method resolution ({SAMPLE} sampled call sites per corpus)")
    db = sqlite3.connect(DB)
    for repo in corpora:
        rows = db.execute(
            """SELECT f.path, s.line, s.col, s.recv FROM call_site s
                 JOIN file f ON f.blob_id = s.blob_id
                 JOIN checkout c ON c.id = f.checkout_id
                WHERE c.root = ? AND f.path NOT LIKE '%test%' AND f.path NOT LIKE '%spec%'
                  AND f.path NOT LIKE 'sorbet/%'
                ORDER BY f.path, line, col""",
            (checkout_root(repo),),
        ).fetchall()
        if len(rows) < SAMPLE:
            continue
        by_rung = collections.Counter()
        by_shape = collections.Counter()
        # Implicit receivers split by what encloses them. Inside a class the
        # enclosing scope IS the receiver; inside a module it is not — whatever
        # includes the module is — so the two have different ceilings, and
        # averaging them hides the finding.
        scope_total = collections.Counter()
        scope_resolved = collections.Counter()
        chain = collections.Counter()
        chain_resolved = collections.Counter()
        typing = collections.Counter()
        no_inference = 0
        for path, line, col, shape in stable_sample(rows, SAMPLE):
            out = trekr(["--def", f"{path}:{line}:{col}", "--json"], repo).stdout
            answer = json.loads(out) if out.strip() else {}
            if answer.get("under") != "call":
                by_rung["not a call at that position"] += 1
                continue
            resolved = answer.get("status") == "resolved"
            if resolved:
                rung = answer.get("resolved_via", "?")
                by_rung["resolved: " + rung] += 1
                if rung in ("self", "const"):
                    no_inference += 1
            else:
                by_rung["residue"] += 1
                by_shape[answer.get("receiver", "?")] += 1

            # Could the lookup even have found the answer? An unresolved
            # ancestor means something the chain needed is not indexed — a gem
            # that is named and absent. Splitting on it is the only way to tell
            # "the ladder failed" from "the index was incomplete".
            complete = not answer.get("unresolved_ancestors")
            chain["complete" if complete else "truncated"] += 1
            if resolved:
                chain_resolved["complete" if complete else "truncated"] += 1

            # And for a local or ivar receiver, say WHY it was typed or not.
            if answer.get("receiver") in ("local", "ivar"):
                if resolved:
                    typing["typed: " + answer.get("resolved_via", "?")] += 1
                else:
                    typing["untyped"] += 1
            if answer.get("receiver") in ("implicit", "self"):
                where = answer.get("receiver_kind") or "no scope (top level)"
                scope_total[where] += 1
                scope_resolved[where] += resolved
        resolved = sum(v for k, v in by_rung.items() if k.startswith("resolved"))
        print(f"\n  {os.path.basename(repo.rstrip('/'))}: "
              f"{100 * resolved / SAMPLE:.0f}% resolved "
              f"({100 * no_inference / SAMPLE:.0f}% with no inference at all)")
        for key, count in sorted(by_rung.items()):
            print(f"    {key:<26} {100 * count / SAMPLE:5.1f}%  ({count})")
        if by_shape:
            shapes = ", ".join(f"{k} {v}" for k, v in by_shape.most_common())
            print(f"    residue by receiver shape: {shapes}")
        for where, count in scope_total.most_common():
            print(f"    self inside a {where:<22} "
                  f"{100 * scope_resolved[where] / count:3.0f}% resolved  "
                  f"({scope_resolved[where]}/{count})")
        for state in ("complete", "truncated"):
            count = chain[state]
            if count:
                print(f"    ancestor chain {state:<22} "
                      f"{100 * chain_resolved[state] / count:3.0f}% resolved  "
                      f"({chain_resolved[state]}/{count})")
        if typing:
            total = sum(typing.values())
            print(f"    local/ivar receivers ({total}):")
            for why, count in typing.most_common():
                print(f"      {why:<24}{count}")


def resolution_rate(corpora):
    """What share of real constant references does the ladder actually resolve?

    Samples stored references and asks `--def` at each position, which is the
    same path a caller takes. Residue is split by whether the name belongs to
    core or a gem, because that is the difference between a known gap and a bug.
    """
    print(f"\n=== constant resolution ({SAMPLE} sampled references per corpus)")
    db = sqlite3.connect(DB)
    for repo in corpora:
        rows = db.execute(
            """SELECT f.path, r.line, r.col, r.name FROM const_ref r
                 JOIN file f ON f.blob_id = r.blob_id
                 JOIN checkout c ON c.id = f.checkout_id
                WHERE c.root = ? AND f.path NOT LIKE '%test%' AND f.path NOT LIKE '%spec%'
                  AND f.path NOT LIKE 'sorbet/%'
                ORDER BY f.path, line, col""",
            (checkout_root(repo),),
        ).fetchall()
        if len(rows) < SAMPLE:
            continue
        tally = collections.Counter()
        unresolved = []
        for path, line, col, name in stable_sample(rows, SAMPLE):
            out = trekr(["--def", f"{path}:{line}:{col}", "--json"], repo).stdout
            answer = json.loads(out) if out.strip() else {}
            if answer.get("under") != "constant":
                tally["not a constant"] += 1
            elif answer.get("status") == "resolved":
                tally["resolved: " + answer.get("resolved_via", "?")] += 1
            else:
                head = name.split("::")[0].lstrip(":")
                known = head in CORE
                tally["residue: core/stdlib" if known else "residue: gem or unknown"] += 1
                if not known:
                    unresolved.append(name)
        resolved = sum(v for k, v in tally.items() if k.startswith("resolved"))
        print(f"\n  {os.path.basename(repo.rstrip('/'))}: "
              f"{100 * resolved / SAMPLE:.0f}% resolved")
        for key, count in sorted(tally.items()):
            print(f"    {key:<26} {100 * count / SAMPLE:5.1f}%  ({count})")
        if unresolved:
            top = ", ".join(n for n, _ in collections.Counter(unresolved).most_common(4))
            print(f"    unresolved names: {top}")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
