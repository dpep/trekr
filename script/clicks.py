#!/usr/bin/env python3
"""Click every identifier in a sample of a repo's files, over the LSP, and
report what came back empty or unsure.

    script/clicks.py REPO [REPO …]         # index, click, tally
    FILES=20 SEED=1 script/clicks.py REPO  # how many files per repo
    LIBRARY=app,lib script/clicks.py APP   # a Rails app's code is in app/

What a person does in an editor, replayed: `textDocument/definition` and
`textDocument/hover` at the first character of every name in the file. The
server logs each miss (DEC-083); this reads them back with `--usage --misses`,
so the harness measures the same record the user's own editor writes.

A store of its own (`TREKR_DB`, default in a temp dir) and `TREKR_USAGE=off`:
it never touches the store an editor is serving from. Point it at copies —
`--index` writes nothing into the checkout, but a harness that reads other
people's repositories should not be the first thing to find out otherwise.

Output: per repo, clicks / empty / unsure for each op; then, over every repo,
the definition misses in buckets (`bucket`) — what the miss is, and whether it
should have resolved, honestly cannot, or is outside the index. The buckets are
read from the server's own reason plus the file and line, so they are a
triage, not a verdict: read a sample of a bucket before routing it.
`MISSES=path` writes every miss as ndjson, for that reading.
"""

import collections, glob, json, os, random, re, subprocess, sys, tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BIN = os.environ.get("TREKR_BIN") or os.path.join(ROOT, "target/release/trekr")
FILES = int(os.environ.get("FILES", "12"))
SEED = int(os.environ.get("SEED", "1"))
OPS = [op for op in os.environ.get("OPS", "definition,hover").split(",") if op]
MISSES = os.environ.get("MISSES")
# The directories read as library code. A gem keeps it in lib/; a Rails app
# mostly in app/ (LIBRARY=app,lib).
LIBRARY = [d for d in os.environ.get("LIBRARY", "lib").split(",") if d]

KEYWORDS = set("""
alias and begin break case class def defined? do else elsif end ensure false for
if in module next nil not or redo rescue retry return self super then true undef
unless until when while yield __FILE__ __LINE__ __method__ require require_relative
private protected public attr_reader attr_writer attr_accessor include extend
prepend raise puts p lambda proc loop
""".split())

NAME = re.compile(r"[@$]{0,2}[A-Za-z_][A-Za-z0-9_]*[?!]?")


def code_spans(line):
    """The parts of a line outside strings and comments — near enough.

    Not a lexer: heredocs, `%w[]` and regexps pass through. A click on a word
    inside one of those is a click a person might make too, and the server
    answers it or says there is no name there.
    """
    out, quote, i = [], None, 0
    start = 0
    while i < len(line):
        c = line[i]
        if quote:
            if c == "\\":
                i += 2
                continue
            if c == quote:
                quote = None
                start = i + 1
        elif c in "'\"":
            out.append((start, line[start:i]))
            quote = c
        elif c == "#":
            break
        i += 1
    if not quote:
        out.append((start, line[start:i]))
    return out


def clicks_in(text):
    """(line, character) of every name's first character, 0-based."""
    for n, line in enumerate(text.split("\n")):
        for offset, span in code_spans(line):
            for match in NAME.finditer(span):
                word = match.group(0)
                # `:sym` and `key:` are names too; keywords are not.
                if word in KEYWORDS:
                    continue
                col = offset + match.start()
                # A method name after `def` is a definition: it answers itself.
                if line[:col].rstrip().endswith("def"):
                    continue
                # ASCII columns: UTF-16 and bytes agree for these files.
                yield n, col


class Lsp:
    def __init__(self, root, env):
        self.proc = subprocess.Popen(
            [BIN, "--lsp"], cwd=root, env=env,
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        )
        self.id = 0

    def send(self, message):
        body = json.dumps(message).encode()
        self.proc.stdin.write(b"Content-Length: %d\r\n\r\n" % len(body) + body)
        self.proc.stdin.flush()

    def read(self):
        length = 0
        while True:
            line = self.proc.stdout.readline()
            if not line:
                raise EOFError("the server exited")
            line = line.strip()
            if not line:
                break
            if line.startswith(b"Content-Length:"):
                length = int(line.split(b":")[1])
        return json.loads(self.proc.stdout.read(length))

    def request(self, method, params):
        self.id += 1
        self.send({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params})
        while True:
            message = self.read()
            if message.get("id") == self.id and "method" not in message:
                return message
            # A server-to-client request (a watch registration) wants a reply.
            if "id" in message and "method" in message:
                self.send({"jsonrpc": "2.0", "id": message["id"], "result": None})

    def notify(self, method, params):
        self.send({"jsonrpc": "2.0", "method": method, "params": params})

    def stop(self):
        self.request("shutdown", None)
        self.notify("exit", None)
        self.proc.stdin.close()
        self.proc.wait()


def is_spec(path):
    return "/spec/" in path or path.endswith("_spec.rb")


def sample(root):
    files = sorted(
        path
        for top in LIBRARY
        for path in glob.glob(os.path.join(root, top, "**/*.rb"), recursive=True)
    )
    specs = sorted(glob.glob(os.path.join(root, "spec/**/*.rb"), recursive=True))
    rng = random.Random(SEED)
    rng.shuffle(files)
    rng.shuffle(specs)
    # Mostly library code, some specs: the two places a person reads.
    lib_n = max(1, FILES * 3 // 4)
    return files[:lib_n] + specs[: FILES - min(lib_n, len(files))]


def run(root, env):
    root = os.path.realpath(root)
    indexed = subprocess.run([BIN, "--index", root], env=env, capture_output=True)
    if indexed.returncode != 0:
        sys.exit(f"--index {root} failed: {indexed.stderr.decode()[-400:]}")
    lsp = Lsp(root, env)
    lsp.request("initialize", {"processId": None, "rootUri": "file://" + root,
                               "capabilities": {}})
    lsp.notify("initialized", {})
    sent = collections.Counter()
    for path in sample(root):
        text = open(path, encoding="utf-8", errors="replace").read()
        uri = "file://" + path
        lsp.notify("textDocument/didOpen", {"textDocument": {
            "uri": uri, "languageId": "ruby", "version": 1, "text": text}})
        for line, character in clicks_in(text):
            at = {"textDocument": {"uri": uri}, "position": {"line": line, "character": character}}
            for op in OPS:
                lsp.request("textDocument/" + op, at)
                sent[op] += 1
                sent[op, is_spec(path)] += 1
        lsp.notify("textDocument/didClose", {"textDocument": {"uri": uri}})
    lsp.stop()
    return sent


def main(repos):
    if not os.path.exists(BIN):
        sys.exit("build first: make release")
    scratch = tempfile.mkdtemp(prefix="trekr-clicks-")
    env = dict(os.environ)
    env.setdefault("TREKR_DB", os.path.join(scratch, "trekr.db"))
    env["TREKR_USAGE"] = "off"
    log = os.path.join(scratch, "lsp.log")
    env["TREKR_LOG"] = log
    out = open(MISSES, "w") if MISSES else None
    everything = []
    totals = collections.Counter()
    for repo in repos:
        before = os.path.getsize(log) if os.path.exists(log) else 0
        sent = run(repo, env)
        totals.update(sent)
        misses = []
        with open(log) as f:
            f.seek(before)
            for line in f:
                event = json.loads(line)
                if event.get("event") == "miss":
                    misses.append(event)
        name = os.path.basename(os.path.realpath(repo))
        print(f"\n{name}")
        for op in OPS:
            mine = [m for m in misses if m["op"] == op]
            empty = sum(1 for m in mine if m["outcome"] == "empty")
            unsure = len(mine) - empty
            total = sent[op] or 1
            print(f"  {op:<11} {sent[op]:>6} clicks   empty {empty:>5} {100 * empty / total:5.1f}%"
                  f"   unsure {unsure:>5} {100 * unsure / total:5.1f}%")
        for m in misses:
            m["repo"] = name
            everything.append(m)
            if out:
                out.write(json.dumps(m) + "\n")
    print(f"\nall repos, {OPS[0]} missed (empty or unsure)")
    for spec, label in ((False, "library"), (True, "spec")):
        total = sum(n for key, n in totals.items() if key == (OPS[0], spec))
        missed = sum(1 for m in everything if m["op"] == OPS[0] and is_spec(m["file"]) == spec)
        print(f"  {label:<8} {total:>6} clicks   missed {missed:>6} {100 * missed / (total or 1):5.1f}%")
    summarize([m for m in everything if m["op"] == OPS[0]])


# What a bucket means for routing: a gap trekr should close, a miss nothing
# static can answer, a name outside the index, or a click on no name at all.
BUCKETS = [
    ("spec DSL and let names", "should resolve"),
    ("let/subject name", "should resolve"),
    ("symbol naming a method", "should resolve"),
    ("compact class path", "should resolve"),
    ("known type, method not found", "should resolve"),
    ("typed, with competitors", "ranking"),
    ("untyped local or parameter", "dynamic"),
    ("chained receiver", "dynamic"),
    ("untyped ivar", "dynamic"),
    ("block DSL (rake, config)", "dynamic"),
    ("symbol argument", "dynamic"),
    ("module never mixed in", "dynamic"),
    ("variable without a visible write", "dynamic"),
    ("unindexed ancestor or constant", "unindexed"),
    ("defined nowhere indexed", "unindexed"),
    ("not a name (symbol, key, literal)", "not code"),
    ("other", "unread"),
]


def bucket(miss):
    why = miss.get("why") or ""
    try:
        line = open(miss["file"], encoding="utf-8", errors="replace").read().split("\n")[miss["line"] - 1]
    except (OSError, IndexError):
        line = ""
    spec = is_spec(miss["file"])
    before = line[: miss["col"] - 1]
    if why == "no name at this position":
        if re.search(r"\b(class|module)\s+[\w:]*$", before):
            return "compact class path"
        return "not a name (symbol, key, literal)"
    if why.startswith("constant") or "unresolved" in why or "ancestors are not indexed" in why:
        return "unindexed ancestor or constant"
    if why.startswith("a variable"):
        return "variable without a visible write"
    if why.startswith(("ambiguous", "low confidence")):
        return "typed, with competitors"
    if "defines this name anywhere" in why:
        return "defined nowhere indexed"
    if "no class the index knows of mixes it in" in why:
        return "module never mixed in"
    if "that include" in why or "that mixes it in defines" in why:
        return "known type, method not found"
    if "the receiver's type is known" in why:
        return "known type, method not found"
    if "receiver symbol" in why:
        if re.search(r"\b(let!?|subject|let_it_be)\(\s*:$", before):
            return "let/subject name"
        if re.search(r"\b(method|instance_method|alias_method|define_method|send|public_send|"
                     r"respond_to\?|private|protected|public|module_function|delegate|"
                     r"attr_\w+)\b[^()]*[( ]\s*[:\w, ]*:$", before):
            return "symbol naming a method"
        return "symbol argument"
    if "receiver implicit" in why:
        return "spec DSL and let names" if spec else "block DSL (rake, config)"
    if "receiver local" in why:
        return "untyped local or parameter"
    if "receiver other" in why:
        return "chained receiver"
    if "receiver ivar" in why:
        return "untyped ivar"
    if "receiver const" in why:
        return "unindexed ancestor or constant"
    return "other"


def summarize(misses):
    if not misses:
        return
    tally = collections.Counter(bucket(m) for m in misses)
    total = len(misses)
    print(f"\nall repos: {total} {OPS[0]} misses, by bucket")
    for group in ("should resolve", "ranking", "dynamic", "unindexed", "not code", "unread"):
        rows = [(name, tally[name]) for name, g in BUCKETS if g == group and tally[name]]
        if not rows:
            continue
        n = sum(c for _, c in rows)
        print(f"  {group:<16} {n:>6}  {100 * n / total:5.1f}%")
        for name, c in sorted(rows, key=lambda r: -r[1]):
            print(f"      {name:<36} {c:>6}  {100 * c / total:5.1f}%")


if __name__ == "__main__":
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    main(sys.argv[1:])
