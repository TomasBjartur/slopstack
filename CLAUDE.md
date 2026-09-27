# Project: a proven, fast blogging platform in Rust

Read `DESIGN.md` first: the goals, the stack, the architecture, the
limits. This file is the working rules.

## Standing goals

- **Faster than Substack** on what readers and writers feel, measured and
  published (including where we lose).
- **Correctness checked as hard as we can, where bugs actually are.** For
  every feature, ask what laws it must obey. Check them in deterministic
  simulation first (the default test; real browsers check the
  simulator's model of the world); make wrong states unwritable with
  types; prove the small, crisp, severe kernels (Verus about the code,
  Lean about models). Each law names a plausible wrong implementation it
  would reject, or is labelled as holding by construction. See
  DESIGN.md, "Correctness: where assurance comes from".
- **Real and non-trivial.** Real auth, real multi-user data, real
  concurrency, production limits and error handling.
- Record results honestly, including negative ones, in `docs/FINDINGS.md`.

## The 10x rule

For every input, name the worst case a real user could produce (see the
table in DESIGN.md; new inputs get a row). Performance tests run at **ten
times** it and must meet the same budgets; tests go past each limit and
require a clear refusal, never corruption. Measure growth too: at N and 10N
the time must grow as the algorithm says. Browser tests also run with the
CPU throttled. Sizes nobody tested are where the first version broke.

## Rules

- **No dependencies at run time** besides vendored SQLite and HACL*. A new
  one needs a written reason in `vendor/README.md` and review. Tools are
  pinned (versions below).
- **Laws live in `spec/` and are owned by people.** A change to `spec/` is
  called out as `SECURITY DECISION:` in the commit message. Laws use their
  own predicates, never the code's. Mutation-test every new law once.
- **Label every guarantee**: proved about the code, proved about a model,
  or tested (how).
- **All I/O goes through the `Io` trait** so the simulator can run it
  (network, clocks, randomness today; the database and injected faults
  are planned: DESIGN.md, "Whole-app simulation").
- **Every loop is bounded, every buffer sized from a named limit** in
  `src/limits.rs`; exceeding one is a handled error.
- **`unsafe` only in `src/sys/`** (syscalls, FFI), each block with a
  comment on why it is sound, checked by Kani where it can be.
- **Data-oriented**: arrays and indexes, not pointer graphs; typed arrays
  in the browser; no allocation in hot loops.
- **Every page feels like an SPA** (DESIGN.md): Datastar on every page for
  server interaction (fragments, optimistic signals), our own JS where the
  browser must own the work, the platform for navigation (speculation
  rules, view transitions, bfcache). No framework, no client router.

## Habits (each from a bug or a lost hour here)

- **Test the second time, not just the first.** Every action on and off,
  twice, and after a reload with the saved preferences. (Datastar sent no
  second request to a URL; reopening in Visual mode broke it; formats
  could be turned on but not off. Twenty suites passed over all three.)
- **A check copied into the browser gets the server's attacks.** Any
  validation in page JavaScript is tested with the same hostile inputs as
  the server's, or removed so the server alone decides. (The open redirect
  was in the page's copy of a check the server did right.)
- **Measure in a used state.** Budgets are checked after a realistic
  sequence of actions, on at least 10 samples, reporting the median and
  the worst. (A fresh page: Vim x 30 ms; after the test's earlier steps:
  65 ms. Four-key samples misled.)
- **Name the unit of every position** (UTF-16 units, characters, bytes)
  in its name or comment. (A test model counting characters drifted after
  the first emoji.)
- **When a test fails, prove whose bug it is first**: reduce it to a
  minimal case, and say "test bug" or "app bug" in the commit. (Several
  failures here were the harness: a const declared twice, a DOM element
  that does not serialize, a stale server on the port.)
- **Deploying**: back up, convert if needed, install, then check the live
  site in a real browser (a temporary session, deleted after) and write
  down the way back (`deploy/README.md`). Ask before each deploy.
- **Measure before fixing, per unit, before and after.** Numbers per key
  or per request, never totals over a sample (a total over ten keys was
  once read as a cost per key). Build the old commit in a worktree and run
  both on the same harness; an estimate is not a measurement (a novel's
  render was guessed at 200 ms and measured at 70).
- **Test steady activity, not only single actions.** Every sync test typed,
  paused, then checked; none saw that a co-author got nothing during three
  seconds of steady typing.
- **A test is done when it fails without the fix.** Break the code on
  purpose once (mutation) and watch the test fail; three new tests passed
  on broken code before they were sharpened (a budget that averaged away
  a one-time cost; a slot-reuse test with no one else parked there).
- **Docs describe what exists.** Anything not built is marked *planned*
  in the same sentence. (DESIGN.md described a whole-server fault
  simulator, and Kani, that did not exist.)
- *Tentative*: a usability pass (driving the real browser as a person
  would, with screenshots) after each larger feature. It found more real
  bugs than the ported suites; it is slow, so perhaps at milestones.

## Toolchains (pinned)

- Rust 1.98.1 (`rustup default 1.98.1`); linker `~/opt/rustlink/cc.sh`
  (this container has no system C toolchain: LLVM 19 in `~/opt`).
- Verus 0.2026.09.20.aef82ed (`~/opt/verus/verus-x86-linux/verus`, with
  its Z3).
- Lean 4.34.1 (`~/.elan/bin/lean`).
- Chrome headless shell for browser tests (`~/opt/chrome-headless-shell-linux64`).

## Checks (all before committing)

- `tools/check.sh`: Verus on every proved module, Lean proofs, the unit
  and property tests, the simulator's seeds, the 10x performance tests,
  the WebAssembly build against the native one, the CRDT against Lean.
- `tools/check_e2e.sh`: every end-to-end suite (HTTP flows and attacks,
  passkeys, sync, red team, workers, real Chrome: browser, collab,
  editor, large, usability, ux; the fuzzer).
- `tests/bench.py` (optional, ~10 min): against the first version, its
  Next.js baseline and a real Substack post; writes `docs/bench.json`.
- Deploying: `deploy/README.md` (ask first: it replaces the live site).

## Status

- Deployed at https://slopstack.tomasbjartur.com (2026-09-27), replacing
  the first version; its data converted with `tools/import_old.py`
  (nothing left behind). The first version's database is kept in
  `~/bent-data` (final copy in `~/bent-data/backups/final-*.db`); going
  back: `deploy/README.md`.
