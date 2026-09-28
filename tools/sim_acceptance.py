#!/usr/bin/env python3
"""The whole-application simulation's acceptance test (DESIGN.md,
"Whole-app simulation"): each bug it should see is put back, one at a
time, the server is built, and seeds are run until one fails; each must be
found within the time budget. For comparison ("before"), the checks that
existed before it (the unit tests and the event loop's simulator) run on
the same broken build.

The bugs are the ones in docs/FINDINGS.md that live where the simulation
reaches, two breaks of the authorization law in the server's code (which
the proofs forbid; the simulation must see them without the proofs), and
two it found itself (kept here so they stay found).

usage: tools/sim_acceptance.py [NAME...]   (the source is restored after
each, and a clean server built at the end)"""
import os, subprocess, sys, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
BUDGET_S = 300          # "within minutes": seeds are run for at most this long
SEEDS = (1, 400)
SECS = 120              # simulated seconds per seed

# (name, file, text as it is, text with the bug, what it was)
BUGS = [
    ("monotonic-dates", "src/server.rs",
     "                let wall = io.wall_ms();",
     "                let wall = io.now_ms();",
     "the application's clock was the monotonic one: dates in 1970, sessions that outlived their expiry"),
    ("reused-ids", "src/db.rs",
     "     CREATE TABLE post (\n       id INTEGER PRIMARY KEY AUTOINCREMENT,",
     "     CREATE TABLE post (\n       id INTEGER PRIMARY KEY,",
     "post ids could be reused: a cached document of a deleted post served as a new one"),
    ("me-bug", "src/site.rs",
     "self.docs.reply(&mut self.st, id, since, mine, &mut body)",
     "self.docs.reply(&mut self.st, id, since, rep, &mut body)",
     "a writer's own batches skipped by the request's replica, not the page's: a reopened page with kept edits"),
    ("stale-answer", "src/server.rs",
     "        if !s.open || !s.parked || s.gen != gen {",
     "        if !s.open || !s.parked {",
     "a parked request's answer given to a later connection in the same slot"),
    ("draft-leak", "src/authz.rs",
     "Action::ReadPost { post } => post_public_exec(f, post) || can_write_post_exec(f, post),",
     "Action::ReadPost { post } => f.post.is_some() || can_write_post_exec(f, post),",
     "(the law broken in the server's code, as a build without proofs could) anyone reads any draft"),
    ("foreign-publish", "src/authz.rs",
     "Action::PublishPost { post } => can_write_post_exec(f, post),",
     "Action::PublishPost { post } => f.who != 0,",
     "(the law broken in the server's code) anyone signed in publishes anyone's post"),
    ("open-transaction", "src/db.rs",
     "            Err(e) => {\n                let _ = self.run(Q::Rollback, &[]);\n                Err(e)\n            }\n        }\n    }\n\n    /// Spends",
     "            Err(e) => Err(e),\n        }\n    }\n\n    /// Spends",
     "(found by the simulation) a failed COMMIT left its transaction open, holding every other worker's writes"),
    ("rolled-back-batch", "src/site.rs",
     "                self.docs.forget(id);\n            }\n            stored.map_err(no_code)?;",
     "            }\n            stored.map_err(no_code)?;",
     "(found by the simulation) a batch rolled back but kept in a worker's copy of the document: its retry was taken for a repeat"),
]


def build():
    verus = os.path.expanduser("~/opt/verus/verus-x86-linux/verus")
    env = dict(os.environ, PATH=os.path.expanduser("~/.cargo/bin") + ":" + os.environ["PATH"])
    r = subprocess.run([verus, "src/main.rs", "--no-verify", "--compile", "-C", "linker=" + os.path.expanduser("~/opt/rustlink/cc.sh"),
                        "-L", "build", "-l", "static=sqlite3", "-l", "static=hacl", "-C", "opt-level=3", "-o", "build/server"],
                       cwd=ROOT, env=env, capture_output=True, text=True)
    if r.returncode != 0:
        print(r.stdout[-2000:], r.stderr[-2000:])
        sys.exit("build failed")


def before():
    """The checks that existed: which of them fail on this build."""
    caught = []
    for t in ["crdt", "db", "http", "sim"]:
        r = subprocess.run([os.path.join(ROOT, "build/server"), "test", t], cwd=ROOT, capture_output=True, text=True, timeout=1200)
        if r.returncode != 0:
            caught.append(t)
    return caught


def main():
    only = sys.argv[1:]
    results = []
    try:
        for name, path, good, bad, what in BUGS:
            if only and name not in only:
                continue
            f = os.path.join(ROOT, path)
            src = open(f, encoding="utf-8").read()
            assert src.count(good) == 1, f"{name}: the fixed text is not in {path} exactly once"
            open(f, "w", encoding="utf-8").write(src.replace(good, bad))
            try:
                build()
                t = time.time()
                old = before()
                old_s = time.time() - t
                t = time.time()
                try:
                    r = subprocess.run([os.path.join(ROOT, "build/server"), "test", "appsim", "sweep", str(SEEDS[0]), str(SEEDS[1]), str(SECS)],
                                       cwd=ROOT, capture_output=True, text=True, timeout=BUDGET_S)
                    out = r.stdout
                except subprocess.TimeoutExpired as e:
                    out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
                    out += "\nNOT FOUND within the budget"
                took = time.time() - t
                found = "FOUND by seed" in out
                first = next((l.strip() for l in out.splitlines() if "VIOLATION" in l), "")
                seed = next((l for l in out.splitlines() if l.startswith("FOUND")), "")
                results.append((name, found, took, seed, first, old, old_s, what))
                print(f"{'CAUGHT' if found else 'MISSED'} {name}: {what}\n  simulation: {seed or 'not found'} ({took:.0f} s)\n  first: {first[:300]}\n"
                      f"  before (unit tests and the event loop's simulator, {old_s:.0f} s): {'caught by ' + ', '.join(old) if old else 'not caught'}", flush=True)
            finally:
                open(f, "w", encoding="utf-8").write(src)
    finally:
        build()
    missed = [r[0] for r in results if not r[1]]
    print(f"\n{len(results) - len(missed)} of {len(results)} caught" + (f"; missed: {', '.join(missed)}" if missed else ""))
    sys.exit(1 if missed else 0)


if __name__ == "__main__":
    main()
