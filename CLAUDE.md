# Project: a proven, fast blogging platform in Rust

Read `DESIGN.md` first: the goals, the stack, the architecture, the
limits. This file is the working rules.

## Standing goals

- **Faster than Substack** on what readers and writers feel, measured and
  published (including where we lose).
- **Correctness checked as hard as we can.** For every feature, ask what
  laws it must obey and prove them (Verus about the code, Lean about
  models); where a proof does not reach, say why and check it another way.
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
- **All I/O goes through the `Io` trait** so the simulator can run it.
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

## Toolchains (pinned)

- Rust 1.98.1 (`rustup default 1.98.1`); linker `~/opt/rustlink/cc.sh`
  (this container has no system C toolchain: LLVM 19 in `~/opt`).
- Verus 0.2026.09.20.aef82ed (`~/opt/verus/verus-x86-linux/verus`, with
  its Z3).
- Lean 4.34.1 (`~/.elan/bin/lean`).
- Chrome headless shell for browser tests (`~/opt/chrome-headless-shell-linux64`).

## Checks (all before committing)

- `tools/check.sh`: Verus on every proved module, Lean proofs, the unit
  and property tests, the simulator's seeds, the 10x performance tests.
