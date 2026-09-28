#!/usr/bin/env python3
"""How good are the tests? Small mutations of the server's code (a
comparison flipped, && and || swapped, true and false, min and max, an
off-by-one, a statement removed), each compiled and run against the
whole-application simulation (and, with --old, the fast checks that came
before it: the unit tests and the event loop's simulator). A mutant a
check fails on is killed; one that survives is equivalent (it changes
nothing that matters: say why in tools/mutants-equivalent.txt) or marks a
blind spot (add the check that kills it, or write it down in FINDINGS).

usage:
  tools/mutants.py --draw N SEED --save FILE   draw a sample of the code as it is
  tools/mutants.py --sample FILE [--old]      run a saved sample (the lines are
                                               found by their text, so a sample
                                               outlives edits elsewhere)
  tools/mutants.py --diff BASE                 every mutant of the lines changed
                                               since commit BASE (before a commit:
                                               see CLAUDE.md, "Simulation workflow")
  tools/mutants.py N SEED [--old]              draw and run (the first measure)
Results in build/mutants.txt; sources are restored after each mutant."""
import json, os, random, re, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FILES = ["src/site.rs", "src/docs.rs", "src/db.rs", "src/crdt.rs", "src/server.rs", "src/notify.rs"]
# (--diff: any server source; the simulation cannot reach the browser's.)
SERVER = re.compile(r"^src/(?!tests/|sim\.rs$|app\.rs$|wasm\.rs$).*\.rs$")
SIM = ["test", "appsim", "sweep", "1", "25", "120"]   # 25 seeds of 2 simulated minutes
OLD = [["test", "crdt"], ["test", "db"], ["test", "http"], ["test", "sim"]]

OPS = [
    (r" == ", " != "), (r" != ", " == "),
    (r" < ", " <= "), (r" <= ", " < "), (r" > ", " >= "), (r" >= ", " > "),
    (r" && ", " || "), (r" \|\| ", " && "),
    (r"\btrue\b", "false"), (r"\bfalse\b", "true"),
    (r"\.max\(", ".min("), (r"\.min\(", ".max("),
    (r" \+ 1\b", ""), (r" - 1\b", ""),
]


def candidates(path):
    """[(line index, old line, new line, what)] for the file's code."""
    lines = open(os.path.join(ROOT, path), encoding="utf-8").read().split("\n")
    out = []
    for i, l in enumerate(lines):
        s = l.strip()
        if not s or s.startswith("//") or s.startswith("#[") or "assert" in s or "println!" in s or "eprintln!" in s:
            continue
        code = l.split("//")[0]
        for pat, rep in OPS:
            for m in re.finditer(pat, code):
                if code[: m.start()].count('"') % 2:
                    continue
                new = code[: m.start()] + rep + code[m.end():] + (l[len(code):] if len(l) > len(code) else "")
                out.append((i, l, new, f"{pat.strip(chr(92)).strip()} -> {rep.strip() or '(removed)'}"))
        if re.match(r"^\s*(self\.|st\.|docs\.|[a-z_]+\.)[a-z_.]+\(.*\);\s*$", l) and not re.match(r"^\s*(let|return)\b", l):
            out.append((i, l, re.match(r"^\s*", l).group(0) + "// (removed by a mutant)", "statement removed"))
    return out


def draw(n, seed, files):
    pool = [dict(file=f, line=i + 1, old=old, new=new, what=what) for f in files for i, old, new, what in candidates(f)]
    random.Random(seed).shuffle(pool)
    return pool[:n] if n else pool


def changed_lines(base):
    """{file: set of line numbers changed since base} (server sources)."""
    out = {}
    d = subprocess.run(["git", "diff", "-U0", base, "--", "src"], cwd=ROOT, capture_output=True, text=True, check=True).stdout
    cur = None
    for l in d.splitlines():
        if l.startswith("+++ "):
            f = l[6:] if l.startswith("+++ b/") else None
            cur = f if f and SERVER.match(f) else None
        elif l.startswith("@@") and cur:
            m = re.search(r"\+(\d+)(?:,(\d+))?", l)
            a, n = int(m.group(1)), int(m.group(2) if m.group(2) is not None else 1)
            out.setdefault(cur, set()).update(range(a, a + n))
    return out


def locate(m):
    """The line index of a saved mutant's line now (by its text, nearest
    its old place), or None if the code there changed."""
    lines = open(os.path.join(ROOT, m["file"]), encoding="utf-8").read().split("\n")
    hits = [i for i, l in enumerate(lines) if l == m["old"]]
    return min(hits, key=lambda i: abs(i + 1 - m["line"])) if hits else None


def build():
    verus = os.path.expanduser("~/opt/verus/verus-x86-linux/verus")
    env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    return subprocess.run([verus, "src/main.rs", "--no-verify", "--compile", "-C", "linker=" + os.path.expanduser("~/opt/rustlink/cc.sh"),
                           "-L", "build", "-l", "static=sqlite3", "-l", "static=hacl", "-C", "opt-level=1", "-o", "build/server-mut"],
                          cwd=ROOT, env=env, capture_output=True, text=True).returncode == 0


def run(args, timeout):
    try:
        r = subprocess.run([os.path.join(ROOT, "build/server-mut")] + args, cwd=ROOT, capture_output=True, text=True, timeout=timeout)
        return r.returncode != 0, r.stdout
    except subprocess.TimeoutExpired:
        return True, "(hung: counted as killed)"


def covered():
    cov = {}
    p = os.path.join(ROOT, "build/cov/sim.lcov")
    if not os.path.exists(p):
        return None
    cur = None
    for l in open(p):
        if l.startswith("SF:"):
            cur = next((l.strip().split(ROOT + "/")[-1] for _ in [0] if ROOT in l), l[3:].strip())
        elif l.startswith("DA:") and cur:
            n, c = l[3:].split(",")[:2]
            cov.setdefault(cur, {})[int(n)] = max(cov.get(cur, {}).get(int(n), 0), int(c))
    return cov


def equivalent():
    """Mutants judged equivalent, with why: 'file|function|line stripped|what => why'."""
    p = os.path.join(ROOT, "tools/mutants-equivalent.txt")
    out = {}
    if os.path.exists(p):
        for l in open(p, encoding="utf-8"):
            if l.strip() and not l.startswith("#") and " => " in l:
                k, why = l.rstrip("\n").split(" => ", 1)
                out[k.strip()] = why.strip()
    return out


def enclosing(path, i):
    """The name of the function around line index i."""
    lines = open(os.path.join(ROOT, path), encoding="utf-8").read().split("\n")
    for j in range(i, -1, -1):
        mm = re.match(r"\s*(pub(\([a-z]+\))? )?(const )?(unsafe )?fn ([A-Za-z_0-9]+)", lines[j])
        if mm:
            return mm.group(5)
    return "?"


def key(m, i):
    return f"{m['file']}|{enclosing(m['file'], i)}|{m['old'].strip()}|{m['what']}"


def main():
    a = sys.argv[1:]
    old = "--old" in a
    a = [x for x in a if x != "--old"]
    if a[:1] == ["--draw"]:
        sample = draw(int(a[1]), int(a[2]), FILES)
        json.dump(sample, open(a[a.index("--save") + 1], "w"), indent=1)
        print(f"{len(sample)} mutants saved")
        return
    if a[:1] == ["--sample"]:
        sample = json.load(open(a[1]))
    elif a[:1] == ["--diff"]:
        ch = changed_lines(a[1])
        sample = [m for m in draw(0, 0, sorted(ch)) if m["line"] in ch[m["file"]]]
        print(f"{len(sample)} mutants of {sum(len(v) for v in ch.values())} changed lines in {len(ch)} files", flush=True)
    else:
        sample = draw(int(a[0]) if a else 40, int(a[1]) if len(a) > 1 else 1, FILES)
    cov, eq = covered(), equivalent()
    log = open(os.path.join(ROOT, "build/mutants.txt"), "w")
    res = []
    for m in sample:
        path = os.path.join(ROOT, m["file"])
        src = open(path, encoding="utf-8").read()
        i = locate(m)
        if i is None:
            line = f"GONE     {m['file']}:{m['line']}  {m['what']}  (that code has changed)  |  {m['old'].strip()[:100]}"
            print(line, flush=True)
            log.write(line + "\n")
            res.append(dict(m, state="gone"))
            continue
        lines = src.split("\n")
        lines[i] = m["new"]
        open(path, "w", encoding="utf-8").write("\n".join(lines))
        try:
            ok = build()
        finally:
            open(path, "w", encoding="utf-8").write(src)
        if not ok:
            res.append(dict(m, state="no build"))
            continue
        sim_k, _ = run(SIM, 300)
        old_k = any(run(x, 600)[0] for x in OLD) if old else None
        ran = cov.get(m["file"], {}).get(i + 1) if cov else None
        why = eq.get(key(m, i))
        state = "killed" if sim_k else ("equivalent" if why else "survived")
        res.append(dict(m, state=state, old_killed=old_k, ran=ran, why=why))
        line = (f"{state.upper():10} {m['file']}:{i + 1}  {m['what']}  ran {ran if ran is not None else '?'}x"
                + (f"  old: {'killed' if old_k else 'survived'}" if old else "") + (f"  ({why})" if why else "")
                + f"  |  {m['old'].strip()[:100]}")
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()
    live = [r for r in res if r["state"] in ("killed", "survived", "equivalent")]
    k = sum(r["state"] == "killed" for r in live)
    e = sum(r["state"] == "equivalent" for r in live)
    summary = (f"\n{len(live)} mutants run ({sum(r['state'] == 'gone' for r in res)} gone, {sum(r['state'] == 'no build' for r in res)} did not build): "
               f"the simulation kills {k} ({100 * k / max(1, len(live)):.0f}%); {e} judged equivalent; "
               f"{k} of {len(live) - e} not equivalent ({100 * k / max(1, len(live) - e):.0f}%)")
    if old:
        ko = sum(bool(r.get("old_killed")) for r in live)
        summary += f"; the fast checks before it kill {ko}, together {sum(r['state'] == 'killed' or bool(r.get('old_killed')) for r in live)}"
    print(summary)
    log.write(summary + "\n")
    surv = [r for r in live if r["state"] == "survived"]
    if a[:1] == ["--diff"] and surv:
        print(f"\n{len(surv)} survivor(s): each needs a check that kills it, a reason it is equivalent "
              "(tools/mutants-equivalent.txt), or a FINDINGS entry as a blind spot.")
        sys.exit(1)


if __name__ == "__main__":
    main()
