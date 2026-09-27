# slopstack

A blogging platform (passkey sign-in, blogs with co-authors, Markdown
posts written in a real-time collaborative editor that works offline,
comments, likes, images), built to be much faster than Substack and
checked as hard as we can: machine-checked proofs about the code that
runs (Verus), a proof about the collaborative text's model (Lean), and
tests for everything the proofs do not reach.

- `DESIGN.md`: the design, and why.
- `docs/FINDINGS.md`: what is guaranteed and how, bugs and what found
  them, performance against the first version, Next.js and Substack,
  and what did not work.
- `CLAUDE.md`: how to work here (rules, toolchains, checks).
- `spec/`: the laws (owned by people), `lean/`: the model proof.
- `deploy/README.md`: deploying.

Build: `tools/build.sh` (server, WebAssembly, editor bundle). Checks:
`tools/check.sh` (proofs and unit tests), then `tools/check_e2e.sh`
(every end-to-end suite, real Chrome included).
