#!/usr/bin/env python3
"""Score every engine that answers goToDefinition against the same runtime truth.

    script/compare.py --engine trekr --gold /tmp/trekr-gold-discourse.ndjson
    script/compare.py --all --gold /tmp/trekr-gold-discourse.ndjson

Session 9's head-to-head hand-picked 45 positions and adjudicated the
disagreements by eye. It answered "who is better today" and could not answer
"are we closing or stalling", because nothing about it was repeatable. This
drives each engine over the **LSP protocol** — the surface an agent actually
uses — against the **TracePoint gold set**, so the scoring is the same
instrument for everyone and a future session re-runs it with one command.

**What is comparable, and what is not.** The other engines return locations with no
status and no confidence: an answer they are sure of and a guess look identical.
So the scored columns are the ones every engine can produce —

    answered    the engine returned at least one location
    correct@1   its *first* location is the file and line Ruby dispatched to
    wrong@1     it answered, and its first location is not that
    found       the truth is anywhere in the locations it returned

— and trekr is scored the same way, by its top location, whether that came from
a resolved answer or the first candidate of a residue. That deliberately throws
away trekr's whole disclosure story so the numbers mean one thing. The status
breakdown lives in BASELINE.md; this file answers a narrower question.

**Setup is timed apart from serving**, because ruby-lsp composes a bundle before
it can answer and hiding that in the first-query number would flatter it as
badly as counting it as latency would condemn it.
"""

import argparse, collections, json, os, random, re, select, shutil, statistics, subprocess, sys, time
from urllib.parse import quote, unquote, urlparse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
GEMS = os.path.expanduser("~/.local/share/trekr-compare")

# The engines under comparison are Ruby programs, so a Ruby has to be on PATH.
# TREKR_RUBY_BIN names one explicitly; otherwise take whichever the shell would.
if os.environ.get("TREKR_RUBY_BIN"):
    RUBY = os.path.expanduser(os.environ["TREKR_RUBY_BIN"])
else:
    _ruby = shutil.which("ruby")
    RUBY = os.path.dirname(_ruby) if _ruby else ""

try:
    sys.stdout.reconfigure(line_buffering=True)
except AttributeError:
    pass


def gem_env(home):
    """A ruby with an isolated gem home, so the corpora's own bundles are untouched."""
    env = dict(os.environ)
    env["GEM_HOME"] = env["GEM_PATH"] = os.path.join(GEMS, home)
    env["PATH"] = f"{os.path.join(GEMS, home, 'bin')}:{RUBY}:{env['PATH']}"
    return env


# Each engine is a command plus what it needs to be told. `setup_is_slow` marks
# the ones whose `initialize` does real work (bundle composition), so the report
# says so rather than leaving a 90-second number unexplained.
ENGINES = {
    "trekr": {
        "version": lambda: run_version([os.path.join(ROOT, "target/release/trekr"), "--version"]),
        "argv": [os.path.join(ROOT, "target/release/trekr"), "--serve"],
        "env": lambda: dict(os.environ),
        "setup_is_slow": False,
    },
    "ruby-lsp": {
        "version": lambda: gem_version("gems", "ruby-lsp"),
        "argv": ["ruby-lsp"],
        "env": lambda: gem_env("gems"),
        "setup_is_slow": True,
    },
    # `--beta` is ruby-lsp's own flag for composing a bundle that accepts
    # pre-release server gems. Without it the composed bundle resolves
    # `gem "ruby-lsp"` to the newest *stable*, and the beta row silently
    # measures the stable one — which it did, identically, before this.
    "ruby-lsp-beta": {
        "version": lambda: gem_version("gems-beta", "ruby-lsp"),
        "argv": ["ruby-lsp", "--beta"],
        "env": lambda: gem_env("gems-beta"),
        "setup_is_slow": True,
    },
    # Sorbet runs out of the *project's* own bundle, not the isolated one:
    # `srb` shells out to `srb-rbi`, which materializes the app's Gemfile, and
    # an isolated gem home cannot. A repo without `sorbet/config` is not a
    # Sorbet project and this engine has nothing to say about it.
    "sorbet": {
        "version": lambda: gem_version("gems", "sorbet-static"),
        "argv": ["bundle", "exec", "srb", "tc", "--lsp", "--disable-watchman"],
        "env": lambda: ruby_env(),
        "setup_is_slow": True,
    },
    # Sorbet's best case, without editing a corpus. At `typed: false` — the
    # default when a file carries no sigil, which is every file in widget_shop
    # and most files in most Rails apps — Sorbet does not resolve method calls
    # at all. `--typed=true` raises the floor for every file, so this row is
    # "Sorbet if the app were annotated", against the row above's "Sorbet as
    # this app configures it". Editing sigils in would have shifted every line
    # by one and invalidated the gold set.
    "sorbet-typed": {
        "version": lambda: gem_version("gems", "sorbet-static"),
        "argv": ["bundle", "exec", "srb", "tc", "--lsp", "--disable-watchman", "--typed=true"],
        "env": lambda: ruby_env(),
        "setup_is_slow": True,
    },
}


def ruby_env():
    """The ambient ruby and its gems — the project's own bundle, untouched.

    Not an isolated `GEM_HOME`: `srb` shells out to `srb-rbi`, which
    materializes the app's whole Gemfile, and a gem home holding only the
    tooling cannot. Sorbet is the one engine that must be measured inside the
    bundle it type-checks.
    """
    env = dict(os.environ)
    env.pop("BUNDLE_GEMFILE", None)
    env["PATH"] = f"{RUBY}:{env['PATH']}"
    return env


def run_version(argv):
    try:
        return subprocess.run(argv, capture_output=True, text=True).stdout.strip() or "?"
    except OSError:
        return "?"


def gem_version(home, gem):
    path = os.path.join(GEMS, home, "gems")
    if not os.path.isdir(path):
        return "?"
    # `ruby-lsp-rspec` starts with `ruby-lsp-` too, so the version part has to
    # look like a version rather than another word.
    pattern = re.compile(rf"^{re.escape(gem)}-(\d[^-]*(?:-[a-z0-9_]+)*)$")
    found = sorted(m.group(1) for m in map(pattern.match, os.listdir(path)) if m)
    return found[-1] if found else "?"


class Lsp:
    """A framed-stdio LSP client. Deliberately minimal — one request at a time."""

    def __init__(self, argv, env, cwd, timeout):
        self.proc = subprocess.Popen(
            argv, cwd=cwd, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self.id = 0
        self.timeout = timeout

    def send(self, method, params, notify=False):
        message = {"jsonrpc": "2.0", "method": method, "params": params}
        if not notify:
            self.id += 1
            message["id"] = self.id
        body = json.dumps(message).encode()
        self.proc.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.proc.stdin.flush()
        return None if notify else self.id

    def read(self):
        """One message off the wire, or None at EOF."""
        length = None
        while True:
            line = self.proc.stdout.readline()
            if not line:
                return None
            line = line.strip()
            if not line:
                break
            if line.lower().startswith(b"content-length:"):
                length = int(line.split(b":")[1])
        if length is None:
            return None
        return json.loads(self.proc.stdout.read(length))

    def answer(self, message):
        """Reply `null` to a server→client request.

        A server that asks for configuration or capability registration and is
        never answered can block; ruby-lsp asks for both. Ignoring them looked
        fine until it did not.
        """
        body = json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": None}).encode()
        self.proc.stdin.write(f"Content-Length: {len(body)}\r\n\r\n".encode() + body)
        self.proc.stdin.flush()

    def request(self, method, params):
        """Send and wait for *this* id, dropping the server's own chatter."""
        want = self.send(method, params)
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            message = self.read()
            if message is None:
                return None
            if message.get("id") == want:
                return message.get("result")
            if "id" in message and "method" in message:
                self.answer(message)
        return None

    def settle(self, budget, quiet_for):
        """Wait until the server has finished preparing to answer.

        ruby-lsp returns `initialize` in seconds and then indexes the workspace
        in the background; Sorbet does the same. Asking before that finishes
        gets an honest empty answer to a question the engine had not been given
        a chance to answer, and publishing it would be the harness's fault
        rather than the engine's. Ends on the first `$/progress` `end`, or on
        silence.
        """
        started = last = time.monotonic()
        heard = False
        while time.monotonic() - started < budget:
            ready, _, _ = select.select([self.proc.stdout], [], [], 1.0)
            if not ready:
                if time.monotonic() - last > quiet_for:
                    break
                continue
            message = self.read()
            last = time.monotonic()
            heard = True
            if message is None:
                break
            if "id" in message and "method" in message:
                self.answer(message)
            elif message.get("method") == "$/progress":
                if message.get("params", {}).get("value", {}).get("kind") == "end":
                    break
        # A server that says nothing was never indexing — it was waiting. Only
        # the silence *after* it spoke is time it spent preparing; reporting
        # the quiet timeout as an engine's index time would invent a cost.
        return round(time.monotonic() - started, 1) if heard else 0.0

    def rss_mb(self):
        try:
            out = subprocess.run(
                ["ps", "-o", "rss=", "-p", str(self.proc.pid)], capture_output=True, text=True
            ).stdout.strip()
            return round(int(out) / 1024) if out else None
        except (OSError, ValueError):
            return None

    def close(self):
        try:
            self.request("shutdown", {})
            self.send("exit", {}, notify=True)
        except (BrokenPipeError, OSError):
            pass
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


def uri(path):
    return "file://" + quote(path)


def path_of(u):
    return unquote(urlparse(u).path) if u else ""


def locations(result):
    """LSP definition results come in three shapes. Flatten to (path, line)."""
    if result is None:
        return []
    items = result if isinstance(result, list) else [result]
    out = []
    for item in items:
        if not isinstance(item, dict):
            continue
        u = item.get("uri") or item.get("targetUri")
        span = item.get("range") or item.get("targetSelectionRange") or item.get("targetRange")
        if not u or not span:
            continue
        out.append((path_of(u), span["start"]["line"] + 1))
    return out


def hits(place, site):
    path, line = place
    try:
        if os.path.realpath(path) != os.path.realpath(site["def_file"]):
            return False
    except OSError:
        return False
    # Same tolerance the gold scorer uses: Ruby reports the `def` keyword's line
    # for some macro-defined methods, one off from where the definition starts.
    return abs(line - int(site["def_line"])) <= 1


LANGUAGE = "ruby"


def score(engine, sites, root, timeout, warmup, args_budget=900, args_quiet=25):
    spec = ENGINES[engine]
    started = time.monotonic()
    client = Lsp(spec["argv"], spec["env"](), root, timeout)
    result = client.request(
        "initialize",
        {
            "processId": os.getpid(),
            "rootUri": uri(root),
            "rootPath": root,
            "workspaceFolders": [{"uri": uri(root), "name": os.path.basename(root)}],
            "capabilities": {
                "textDocument": {
                    "definition": {"linkSupport": True},
                    "synchronization": {"didSave": False},
                }
            },
        },
    )
    setup = time.monotonic() - started
    if result is None:
        client.close()
        return {"engine": engine, "error": "initialize timed out or the server died"}
    client.send("initialized", {}, notify=True)
    # The server's own answer beats reading a gem directory: ruby-lsp composes
    # the *project's* bundle, so the version that runs is the one discourse
    # pins, not the one installed here.
    reported = (result.get("serverInfo") or {}).get("version")
    indexing = client.settle(args_budget, args_quiet)

    opened = set()
    verdicts = collections.Counter()
    found = 0
    latencies = []
    cold = None

    def open_file(path):
        if path in opened:
            return True
        try:
            text = open(path, encoding="utf-8", errors="replace").read()
        except OSError:
            return False
        client.send(
            "textDocument/didOpen",
            {"textDocument": {"uri": uri(path), "languageId": LANGUAGE, "version": 1, "text": text}},
            notify=True,
        )
        opened.add(path)
        return True

    # Sorbet and ruby-lsp answer from indexed state that is still being built
    # when `initialize` returns. A discarded warm-up request is fairer than
    # scoring an engine on answers it had not finished preparing.
    if warmup and sites:
        open_file(sites[0]["file"])
        client.request(
            "textDocument/definition",
            {
                "textDocument": {"uri": uri(sites[0]["file"])},
                "position": {"line": sites[0]["line"] - 1, "character": sites[0]["col"] - 1},
            },
        )

    for i, site in enumerate(sites, 1):
        if not open_file(site["file"]):
            verdicts["unreadable"] += 1
            continue
        at = time.monotonic()
        answer = client.request(
            "textDocument/definition",
            {
                "textDocument": {"uri": uri(site["file"])},
                "position": {"line": site["line"] - 1, "character": site["col"] - 1},
            },
        )
        elapsed = (time.monotonic() - at) * 1000
        if cold is None:
            cold = elapsed
        else:
            latencies.append(elapsed)
        places = locations(answer)
        if not places:
            verdicts["none"] += 1
        elif hits(places[0], site):
            verdicts["correct"] += 1
        else:
            verdicts["wrong"] += 1
        if any(hits(place, site) for place in places):
            found += 1
        if i % 100 == 0:
            print(f"    … {i}/{len(sites)}")

    rss = client.rss_mb()
    client.close()
    scored = sum(verdicts.values()) or 1
    return {
        "engine": engine,
        "version": reported or spec["version"](),
        "date": time.strftime("%Y-%m-%d"),
        "scored": sum(verdicts.values()),
        "answered_pct": round(100 * (verdicts["correct"] + verdicts["wrong"]) / scored, 1),
        "correct_pct": round(100 * verdicts["correct"] / scored, 1),
        "wrong_pct": round(100 * verdicts["wrong"] / scored, 1),
        "found_pct": round(100 * found / scored, 1),
        "setup_s": round(setup, 2),
        "indexing_s": indexing,
        "setup_is_slow": spec["setup_is_slow"],
        "cold_ms": round(cold, 1) if cold else None,
        "warm_median_ms": round(statistics.median(latencies), 2) if latencies else None,
        "rss_mb": rss,
    }


def checkout_root(files):
    """The checkout the traced files sit in — not their common directory.

    `commonpath` of the app sites is `<root>/app`, and rewriting *that* silently
    drops the `app/` segment and every file goes missing. The verdicts all
    landed in one benign bucket, which is the failure mode this project keeps
    relearning.
    """
    here = os.path.commonpath(files)
    while here != "/":
        if os.path.exists(os.path.join(here, ".git")):
            return here
        here = os.path.dirname(here)
    return os.path.commonpath(files)


def load(gold, scope, sample, seed, root):
    sites = [json.loads(line) for line in open(gold)]
    sites = [s for s in sites if s.get("def_file", "").endswith(".rb") and s.get("scope") == scope]
    if root:
        # The gold set was traced in one checkout; an engine may be pointed at a
        # **worktree** of it — widget_shop and widget_shop-nosorbet differ only
        # by a `sorbet/` directory their app code never mentions. Both the call
        # site and a truth that lives in the app move with it; a truth inside a
        # gem does not, and must not.
        old = checkout_root([s["file"] for s in sites])
        moved = []
        for site in sites:
            site = dict(site)
            site["file"] = site["file"].replace(old, root, 1)
            if site["def_file"].startswith(old + "/"):
                site["def_file"] = site["def_file"].replace(old, root, 1)
            moved.append(site)
        sites = moved
    if sample and len(sites) > sample:
        random.Random(seed).shuffle(sites)
        sites = sites[:sample]
    return sites


ROW = ("| {engine} | {version} | {date} | {corpus} | {scored} | {answered_pct} % | "
       "{correct_pct} % | {found_pct} % | {wrong_pct} % | {setup} | {cold_ms} ms | "
       "{warm} | {rss} |")


def render(result, corpus):
    if result.get("error"):
        return f"| {result['engine']} | — | — | {corpus} | — | {result['error']} |"
    setup = f"{result['setup_s']} s + {result['indexing_s']} s index"
    warm = f"{result['warm_median_ms']} ms" if result["warm_median_ms"] else "—"
    rss = f"{result['rss_mb']} MB" if result["rss_mb"] else "—"
    fields = {**result, "corpus": corpus, "setup": setup, "warm": warm, "rss": rss}
    return ROW.format(**fields)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--gold", default="/tmp/trekr-gold-discourse.ndjson")
    parser.add_argument("--engine", action="append", default=[])
    parser.add_argument("--all", action="store_true")
    parser.add_argument("--root", required=True, help="workspace root each engine is pointed at")
    parser.add_argument("--rewrite-root", default=None, help="rewrite gold call-site paths to this root")
    parser.add_argument("--scope", default="app")
    parser.add_argument("--sample", type=int, default=500)
    parser.add_argument("--seed", type=int, default=12)
    parser.add_argument("--timeout", type=float, default=90.0)
    parser.add_argument("--no-warmup", action="store_true")
    parser.add_argument("--settle-budget", type=float, default=900.0,
                        help="how long to let the server finish indexing before asking")
    parser.add_argument("--settle-quiet", type=float, default=25.0,
                        help="seconds of silence that count as finished indexing")
    parser.add_argument("--corpus", default=None, help="label for the corpus column")
    parser.add_argument("--out", default=None, help="append one JSON object per engine here")
    args = parser.parse_args()

    engines = list(ENGINES) if args.all else (args.engine or ["trekr"])
    sites = load(args.gold, args.scope, args.sample, args.seed, args.rewrite_root)
    corpus = args.corpus or os.path.basename(args.root.rstrip("/"))
    print(f"{len(sites)} {args.scope} sites, seed {args.seed}, root {args.root}\n")

    rows = []
    for engine in engines:
        print(f"  {engine} …")
        result = score(engine, sites, args.root, args.timeout, not args.no_warmup,
                       args.settle_budget, args.settle_quiet)
        result["corpus"] = corpus
        result["seed"] = args.seed
        rows.append(result)
        print("    " + json.dumps(result))
        if args.out:
            with open(args.out, "a") as out:
                out.write(json.dumps(result) + "\n")

    print("\n| engine | version | date | corpus | sites | answered | correct@1 | found | wrong@1 |"
          " setup | cold | warm median | RSS |")
    print("| --- | --- | --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |")
    for result in rows:
        print(render(result, corpus))


if __name__ == "__main__":
    main()
