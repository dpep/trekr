#!/usr/bin/env python3
"""Score `--dead`'s example group member rows against a runtime gold set
written by script/trace_lets.rb (DEC-492).

    script/dead_lets.py LETS.ndjson DEAD.json CHECKOUT [--split fitted|held|all] [--show used|unused|missed]

Precision is per kind, tier and confidence: of the rows whose member's group
ran a passing example, how many were never called. Recall is how many such
never-called `let`s, `subject`s and `def`s trekr lists (`let!` and
`subject!` aside: they run for every example). `--split held` keeps a
stable half of the spec directories, chosen by a hash of the directory's
name, that no rule was fitted on; `fitted` the other half.
"""
import json, sys, collections, hashlib
gold_path, dead_path, repo = sys.argv[1:4]
split = 'all'
show = None
args = sys.argv[4:]
if '--split' in args: split = args[args.index('--split')+1]
if '--show' in args: show = args[args.index('--show')+1]

def part(path):
    # held out: a stable half of spec directories (by the directory under spec/)
    parts = path.split('/')
    key = '/'.join(parts[:3]) if len(parts) > 3 else '/'.join(parts[:2])
    h = int(hashlib.md5(key.encode()).hexdigest(), 16)
    return 'held' if h % 2 else 'fitted'

lines_cache = {}
def line_text(path, line):
    if path not in lines_cache:
        try: lines_cache[path] = open(f"{repo}/{path}").read().split('\n')
        except Exception: lines_cache[path] = []
    ls = lines_cache[path]
    return ls[line-1] if 0 < line <= len(ls) else ''

gold = {}
for l in open(gold_path):
    r = json.loads(l)
    if r['type'] == 'shared': continue
    key = (r['path'], r['line'], r['name'])
    g = gold.setdefault(key, {'type': r['type'], 'ran': False, 'hits': 0})
    g['ran'] |= r['ran']; g['hits'] += r['hits']

def truth(path, line, name):
    g = gold.get((path, line, name))
    if g is None: return 'missing'
    if not g['ran']: return 'unknown'
    return 'unused' if g['hits'] == 0 else 'used'

dead = json.load(open(dead_path))
rows = [r for r in dead['candidates'] if r.get('group') is not None]
root = repo.rstrip('/') + '/'
table = collections.Counter()
listed = set()
shown = []
for r in rows:
    path = r['path'].replace(root, '')
    if split != 'all' and part(path) != split: continue
    t = truth(path, r['line'], r['name'])
    listed.add((path, r['line'], r['name']))
    kind = r['kind']
    table[(kind, r['tier'], r['confidence'], t)] += 1
    if show and t == show:
        shown.append(f"{path}:{r['line']} {kind} {r['name']} {r['tier']} {r['confidence']} {r['caveat']}")

print(f"split={split}")
for kind in ['let', 'subject', 'method']:
    for tier in ['unreferenced', 'shadowed']:
        for conf in ['clear', 'lower']:
            c = {t: table[(kind, tier, conf, t)] for t in ['unused', 'used', 'unknown', 'missing']}
            n = c['unused'] + c['used']
            if sum(c.values()) == 0: continue
            prec = f"{c['unused']}/{n} = {100*c['unused']/n:.0f} %" if n else '-'
            print(f"{kind:8} {tier:13} {conf:6} precision {prec:16} unknown {c['unknown']:4} missing {c['missing']}")

# recall: truly unused lets (not let!, not in a failing/unrun group) trekr lists
unused = collections.Counter(); found = collections.Counter()
for (path, line, name), g in gold.items():
    if split != 'all' and part(path) != split: continue
    if not g['ran'] or g['hits'] or not path.startswith('spec/'): continue
    text = line_text(path, line)
    if 'let!(' in text or 'subject!' in text or 'before' in text: continue
    # A method a helper macro makes on a group (`define_method`) is no
    # group `def` trekr weighs.
    if g['type'] == 'def' and not text.strip().startswith('def '): continue
    kind = 'def' if g['type'] == 'def' else ('subject' if 'subject' in text else 'let')
    unused[kind] += 1
    if (path, line, name) in listed: found[kind] += 1
    elif show == 'missed': shown.append(f"{path}:{line} {kind} {name}  | {text.strip()[:90]}")
for kind in unused:
    print(f"recall {kind:8} {found[kind]}/{unused[kind]} = {100*found[kind]/unused[kind]:.0f} %")
for s in shown: print(s)
