#!/usr/bin/env python3
"""How good are the tests? Random small mutations of the server's code
(a comparison flipped, && and || swapped, true and false, min and max, an
off-by-one, a statement removed), each compiled and run against the
whole-application simulation and, for comparison, the fast checks that
came before it (the unit tests and the event loop's simulator). A mutant
a check fails on is killed; one nobody kills either is equivalent (it
changes nothing that matters) or marks a blind spot. Survivors are
listed with whether the simulation even runs their line (coverage, from
build/cov/sim.lcov if present).

usage: tools/mutants.py N [SEED] [FILE...]   (sources restored after each)
writes build/mutants.txt"""
import os, random, re, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FILES = ["src/site.rs", "src/docs.rs", "src/db.rs", "src/crdt.rs", "src/server.rs", "src/notify.rs"]
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
    """(line index, new line, what) for every mutation of the file's code."""
    lines = open(os.path.join(ROOT, path), encoding="utf-8").read().split("\n")
    out = []
    in_verus_spec = False
    for i, l in enumerate(lines):
        s = l.strip()
        if not s or s.startswith("//") or s.startswith("#[") or "assert" in s or "debug_assert" in s or "println!" in s or "eprintln!" in s:
            continue
        if "=>" in s and ('"' in s) and s.startswith('"'):
            continue
        code = l.split("//")[0]
        for pat, rep in OPS:
            for m in re.finditer(pat, code):
                # (Not inside a string literal: an even number of quotes before.)
                if code[: m.start()].count('"') % 2:
                    continue
                new = code[: m.start()] + rep + code[m.end():] + (l[len(code):] if len(l) > len(code) else "")
                out.append((i, new, f"{pat.strip(chr(92)).strip()} -> {rep.strip() or '(removed)'}"))
        # A statement removed: a call ending the line, not a binding or return.
        if re.match(r"^\s*(self\.|st\.|docs\.|[a-z_]+\.)[a-z_.]+\(.*\);\s*$", l) and not re.match(r"^\s*(let|return)\b", l):
            out.append((i, re.match(r"^\s*", l).group(0) + "// (removed by a mutant)", "statement removed"))
    return lines, out


def run(args, timeout):
    t = time.time()
    try:
        r = subprocess.run([os.path.join(ROOT, "build/server-mut")] + args, cwd=ROOT, capture_output=True, text=True, timeout=timeout)
        return r.returncode != 0, time.time() - t, r.stdout
    except subprocess.TimeoutExpired:
        return True, time.time() - t, "(hung: counted as killed)"


def covered_lines():
    cov = {}
    p = os.path.join(ROOT, "build/cov/sim.lcov")
    if not os.path.exists(p):
        return None
    cur = None
    for l in open(p):
        if l.startswith("SF:"):
            cur = next((f for f in FILES if l.strip().endswith("/" + f)), None)
        elif l.startswith("DA:") and cur:
            n, c = l[3:].split(",")[:2]
            cov.setdefault(cur, {})[int(n)] = max(cov.get(cur, {}).get(int(n), 0), int(c))
    return cov


def main():
    n = int(sys.argv[1]) if len(sys.argv) > 1 else 40
    seed = int(sys.argv[2]) if len(sys.argv) > 2 else 1
    files = sys.argv[3:] or FILES
    rng = random.Random(seed)
    pool = []
    for f in files:
        lines, cs = candidates(f)
        pool += [(f, i, new, what) for i, new, what in cs]
    rng.shuffle(pool)
    cov = covered_lines()
    verus = os.path.expanduser("~/opt/verus/verus-x86-linux/verus")
    env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    results = []
    log = open(os.path.join(ROOT, "build/mutants.txt"), "w")
    tried = 0
    for f, i, new, what in pool:
        if len(results) >= n:
            break
        tried += 1
        path = os.path.join(ROOT, f)
        src = open(path, encoding="utf-8").read()
        lines = src.split("\n")
        old_line = lines[i]
        lines[i] = new
        open(path, "w", encoding="utf-8").write("\n".join(lines))
        try:
            b = subprocess.run([verus, "src/main.rs", "--no-verify", "--compile", "-C", "linker=" + os.path.expanduser("~/opt/rustlink/cc.sh"),
                                "-L", "build", "-l", "static=sqlite3", "-l", "static=hacl", "-C", "opt-level=1", "-o", "build/server-mut"],
                               cwd=ROOT, env=env, capture_output=True, text=True)
        finally:
            open(path, "w", encoding="utf-8").write(src)
        if b.returncode != 0:
            continue
        sim_k, sim_t, sim_out = run(SIM, 300)
        old_k = any(run(a, 600)[0] for a in OLD)
        c = cov.get(f, {}).get(i + 1) if cov else None
        where = f"{f}:{i + 1}"
        r = (where, what, sim_k, old_k, c, old_line.strip(), new.strip())
        results.append(r)
        line = f"{'KILLED ' if sim_k else 'SURVIVED'} sim / {'killed ' if old_k else 'survived'} old  {where}  {what}  ran {c if c is not None else '?'}x  |  {old_line.strip()[:110]}"
        print(line, flush=True)
        log.write(line + "\n")
        log.flush()
    k_sim = sum(r[2] for r in results)
    k_old = sum(r[3] for r in results)
    k_any = sum(r[2] or r[3] for r in results)
    reached = [r for r in results if r[4]]
    summary = (f"\n{len(results)} mutants that compile (of {tried} tried): the simulation kills {k_sim} ({100 * k_sim / max(1, len(results)):.0f}%), "
               f"the fast checks before it {k_old} ({100 * k_old / max(1, len(results)):.0f}%), together {k_any}; "
               f"of the {len(reached)} on lines the simulation runs, it kills {sum(r[2] for r in reached)}")
    print(summary)
    log.write(summary + "\n")


if __name__ == "__main__":
    main()
