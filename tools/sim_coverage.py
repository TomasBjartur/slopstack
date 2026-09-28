#!/usr/bin/env python3
"""The whole-application simulation's line coverage of the server, held
to a ratchet: builds the server with coverage (LLVM's, from the Rust
toolchain: `rustup component add llvm-tools`), runs the simulation's seed
budget, and compares each file's share of lines run with
tools/coverage-baseline.txt. A file that falls more than half a point
below its baseline fails the check; one that rises is reported (then
--update raises the baseline: it only goes up).

usage: tools/sim_coverage.py [--update]
writes build/cov/sim.lcov (tools/mutants.py reads it: which lines ran)"""
import glob, os, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FILES = ["src/site.rs", "src/docs.rs", "src/db.rs", "src/crdt.rs", "src/server.rs", "src/notify.rs",
         "src/webauthn.rs", "src/http.rs", "src/pages.rs", "src/form.rs", "src/resp.rs", "src/authz.rs"]
BASELINE = os.path.join(ROOT, "tools/coverage-baseline.txt")


def tool(name):
    out = subprocess.run([os.path.expanduser("~/.cargo/bin/rustc"), "--print", "sysroot"], capture_output=True, text=True).stdout.strip()
    hits = glob.glob(os.path.join(out, "lib/rustlib/*/bin", name))
    if not hits:
        sys.exit(f"{name} not found: rustup component add llvm-tools")
    return hits[0]


def main():
    update = "--update" in sys.argv
    env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    verus = os.path.expanduser("~/opt/verus/verus-x86-linux/verus")
    b = subprocess.run([verus, "src/main.rs", "--no-verify", "--compile", "-C", "linker=" + os.path.expanduser("~/opt/rustlink/cc.sh"),
                        "-L", "build", "-l", "static=sqlite3", "-l", "static=hacl", "-C", "opt-level=1", "-C", "instrument-coverage",
                        "-o", "build/server-cov"], cwd=ROOT, env=env, capture_output=True, text=True)
    if b.returncode != 0:
        sys.exit("the coverage build failed:\n" + b.stderr[-2000:])
    cov = os.path.join(ROOT, "build/cov")
    os.makedirs(cov, exist_ok=True)
    for f in glob.glob(os.path.join(cov, "*.profraw")):
        os.remove(f)
    r = subprocess.run([os.path.join(ROOT, "build/server-cov"), "test", "appsim"], cwd=ROOT, capture_output=True, text=True,
                       env=dict(os.environ, LLVM_PROFILE_FILE=os.path.join(cov, "sim-%p.profraw")))
    if r.returncode != 0:
        sys.exit("the simulation failed under coverage:\n" + r.stdout[-2000:])
    subprocess.run([tool("llvm-profdata"), "merge", "-sparse", *glob.glob(os.path.join(cov, "*.profraw")), "-o", os.path.join(cov, "sim.profdata")], check=True)
    lcov = subprocess.run([tool("llvm-cov"), "export", os.path.join(ROOT, "build/server-cov"), "-instr-profile=" + os.path.join(cov, "sim.profdata"),
                           "-format=lcov"], capture_output=True, text=True, check=True).stdout
    open(os.path.join(cov, "sim.lcov"), "w").write(lcov)
    # Lines run, per file (a line counts once, however many copies of its
    # function the compiler made).
    hit, seen, cur = {}, {}, None
    for l in lcov.splitlines():
        if l.startswith("SF:"):
            cur = next((f for f in FILES if l.endswith("/" + f)), None)
        elif l.startswith("DA:") and cur:
            n, c = l[3:].split(",")[:2]
            seen.setdefault(cur, set()).add(int(n))
            if int(c) > 0:
                hit.setdefault(cur, set()).add(int(n))
    now = {f: 100.0 * len(hit.get(f, ())) / max(1, len(seen.get(f, ()))) for f in FILES}
    base = {}
    if os.path.exists(BASELINE):
        for l in open(BASELINE):
            if l.strip() and not l.startswith("#"):
                f, p = l.split()
                base[f] = float(p)
    worse = []
    for f in FILES:
        b0 = base.get(f)
        mark = "" if b0 is None else ("  FELL from %.1f" % b0 if now[f] < b0 - 0.5 else ("  rose from %.1f" % b0 if now[f] > b0 + 0.5 else ""))
        if b0 is not None and now[f] < b0 - 0.5:
            worse.append(f)
        print(f"  {f:18} {now[f]:5.1f}% of {len(seen.get(f, ())):5} lines{mark}")
    total = 100.0 * sum(len(hit.get(f, ())) for f in FILES) / max(1, sum(len(seen.get(f, ())) for f in FILES))
    print(f"  all                {total:5.1f}%")
    if update:
        keep = {f: max(now[f], base.get(f, 0.0)) for f in FILES}
        with open(BASELINE, "w") as out:
            out.write("# The whole-application simulation's line coverage (%), per file: a ratchet (tools/sim_coverage.py).\n")
            for f in FILES:
                out.write(f"{f} {keep[f]:.1f}\n")
        print("baseline updated")
    if worse:
        sys.exit("coverage fell below its baseline in: " + ", ".join(worse) + " (the simulation no longer runs code it did: add the action back, or say why and lower the baseline by hand)")


if __name__ == "__main__":
    main()
