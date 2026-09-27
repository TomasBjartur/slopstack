# Findings

What the second version (Rust + Verus + Lean, zero run-time crates) set
out to show, what it showed, and what went wrong. Numbers are from this
machine (2 vCPUs, 4 GB) unless said otherwise.

## What is guaranteed, and how

Every guarantee is labelled: **proved about the code** (Verus checks the
Rust that runs), **proved about a model** (Lean, about a definition the
code is tested against), or **tested** (how).

| Law | Where | Kind |
|---|---|---|
| The HTTP head parser never reads out of bounds; heads, header counts and targets are limited; what it accepts is `head_ok` | `spec/http.rs`, `src/http.rs` | proved about the code |
| Authorization: an action happens only with a `Permit`, which only `authorize` makes, and only when the policy allows it; the facts it was decided on are re-read inside the write's transaction | `spec/authz.rs`, `src/authz.rs`, `src/db.rs` `Store::write` | policy proved about the code; the re-check tested (`tests/redteam_test.py`) |
| CSRF: nothing changes state without `Sec-Fetch-Site: same-origin` | `spec/authz.rs` `csrf_ok`, `src/authz.rs` `csrf` | proved about the code; every POST route passes it first (tested) |
| Write budget: at most 30 writes a minute per user | `spec/authz.rs` `within_budget` | the decision proved; the counting tested (60 writes at once make exactly 30) |
| No XSS: every byte of rendered Markdown is allowed markup (tags from a list, text without `<` `>` `"`, URLs with allowed schemes, `&` escaped) | `spec/markup.rs`, `src/html.rs` | proved about the code, for any input |
| The same renderer in the browser (Visual mode) | `src/wasm.rs` | the same code compiled to WebAssembly (proofs erased as in Verus's own build); tested equal to the server's on all 652 CommonMark examples and random inputs |
| Passkeys: a registration or login is accepted only if WebAuthn L3 7.1/7.2's checks pass | `spec/webauthn.rs`, `src/webauthn.rs` | proved about the code (the decision); parsing tested (71 flows and attacks, a software authenticator, and real Chrome) |
| Sessions: none is issued without a passkey check that passed | `Store::session_for` takes a `webauthn::Passed`, whose field is private and made only by `checked(decision)` | by construction (Rust privacy), not a Verus proof |
| Every page's text is escaped or template text | `src/pages.rs` `H`: `r` takes only `&'static str`, `t` escapes (proved escaper) | by construction (types) |
| Collaborative text: replicas with the same operations show the same text, whatever the order or repeats | `lean/Fugue.lean` `converge`, `text_congr`, `merge_*` | proved about a model |
| The Rust CRDT is that model | `src/crdt.rs` | tested: 410 random multi-replica histories against a naive tree walk (views, snapshots, repeats), 200 against the Lean definitions themselves (`tests/crdt_lean.sh`); mutations caught |
| Only the writer's own replica number makes new elements | `src/docs.rs` `store`, `doc_rep` | tested (`tests/sync_test.py`, red team) |

What is trusted and not proved: SQLite and HACL* (vendored; HACL* is
itself formally verified), the Rust compiler and standard library,
`src/sys/` (hand-declared syscalls and FFI, the only `unsafe`), the
browser, Caddy.

## Tests (all pass at the time of writing)

| Suite | What | Size |
|---|---|---|
| `build/server test http` | parser: property tests, 10x heads | growth checked N vs 10N |
| `build/server test sim` | the event loop under a seeded network: splits, slow readers and senders, floods, 100,000 connections | 11,530 checks |
| `build/server test markdown` | CommonMark 0.31.2 | 576 exact, 76 deviations by law (raw HTML shown as text; unsafe schemes), 0 failures |
| `build/server test crdt` | the CRDT against the model, bad input, 10x budgets | 410 histories |
| `tests/crdt_lean.sh` | Rust against the Lean model's definitions | 200 histories |
| `tests/wasm_test.mjs` | the WebAssembly build against the server's | 652 + 100 renders |
| `tests/app_test.py` | flows and attacks over HTTP | 115 |
| `tests/passkey_test.py` | WebAuthn with a software authenticator | 71 |
| `tests/sync_test.py` | the editor's sync protocol and its attacks | 29 |
| `tests/redteam_test.py` | every kind of user against every route | 235 |
| `tests/browser_test.py` | Chrome with a virtual authenticator: sign-up, login, recovery, writing | 31 |
| `tests/collab_test.py` | two Chromes, two users, one document, offline | 16 |
| `tests/editor_test.py` | Vim, Visual mode, round trips | 32 |
| `tests/large_test.py` | 1M and 3M characters in the editor | 50 |
| `tests/workers_test.py` | 4 worker processes: freshness, documents, supervision | 7 |
| `tests/fuzz_server.py` | mutated requests at a live server | 80,000 requests |

## Bugs, and what found them

| Bug | Found by | Would a proof have caught it? |
|---|---|---|
| HACL*'s `uncompressed_to_raw` does not check the point is on the curve | a unit test | no: it is trusted code |
| Strict JSON accepted a repeated key (`"type"` twice in clientDataJSON) | the old passkey attack test | no: parsing is tested, not proved |
| Attestation statements other than `none` were ignored, not refused | the old passkey attack test | no (same) |
| The application stored monotonic time (ms since boot): dates in 1970, expired sessions still valid | the old red-team test, ported | no: the laws are about who, not when. The simulator could not see it (its clock is fake either way) |
| Deleting a blog left its posts' documents in memory; SQLite reused the post id; a new post by someone else showed the old text | the red-team test | no: authorization was right (the new owner may read their post); the cache was keyed by a reusable id. Fixed at the root: ids are never reused |
| Statement ids offset by the migration's own statement | the first end-to-end run | — |
| A slowloris could lock readers out of the head buffers; evicting a connection lost its pending output; memory at 100K connections | the simulator | no |

As in the first version: proofs held where aimed (no authorization,
markup or parser bug was found in proved code); the bugs lived around
them. The ported browser and red-team suites, written against the first
version, found the two worst bugs in this one within minutes.

## What changed from the first version, and why

- **No stored HTML, no render plans, no per-block storage.** They existed
  because Bend strings cost ~20-35 ns a character. Rust renders a 6 MB
  novel in ~200 ms, cached in memory by the post's update time.
- **The CRDT works on runs.** A paste of a novel is one operation (60 MB
  in 162 ms), not six million; typing continues a run.
- **One CRDT and one renderer on both sides**: the server's Rust compiled
  to WebAssembly (245 KB, 89 KB compressed), not a second hand-written JS
  CRDT kept honest by fuzzing.
- **Visual mode's 6,000-character block limit is gone** (a workaround for
  the old JS renderer's recursion): 1.5 MB, ten times the longest
  paragraph we could imagine.
- **Ids never reused** (the fix above).

## Negative results and costs

- **Verus cannot build for WebAssembly out of the box.** The release ships
  `vstd` for the host only; `tools/build_wasm.sh` builds it from the Verus
  sources at the pinned commit (with `VSTD_KIND=IsVstd`).
- **WebAssembly needs `'wasm-unsafe-eval'` in the CSP.** Only the edit page
  has it; it allows compiling WebAssembly, not `eval`.
- **Datastar applies only 200 answers.** A refused Datastar request is a
  200 with a notice patched in; the status is in the notice, not the line.
- **The model proof is about sets, so it is easy**; the hard part (the
  efficient code equals the model) is tested, not proved. Proving the run
  structure's in-order walk equal to the model in Verus is future work.
- **One very long paragraph was slow to type in (fixed).** The editor's
  view kept a window of the text in the textarea, cut at line breaks
  only, so a 300 KB paragraph sat in it whole: 155-192 ms a key, 101 ms
  of it the browser's layout (measured with Chrome's performance
  metrics; our script was 3 ms). The window now also cuts inside lines
  longer than 16 KB, at a space: 11-18 ms a key at 300 KB and 14.5 ms at
  1.5 MB (the 10x case). The cost: such a paragraph shows a break where
  the window starts or ends. Tested: a budget, and 60 random edits across
  the cuts leave the page, the view and the document equal.
- **Real-time is polling** (0.4 s while others type, 1.5 s otherwise),
  not push: simple across worker processes; latency is visible.
- **Left behind in the conversion from the first version**: comments,
  likes, images, tags, schedules, custom domains (not in this version's v1).
- **Kani is not used yet.** The design planned bounded model checking of
  `src/sys/` (the `unsafe` syscall and FFI wrappers). They are small and
  exercised by every test, but not model-checked.
- **Per-worker rate limits** (login challenges, editor page loads) are per
  process: with N workers the limit is N times higher.

## Performance

`tests/bench.py` (raw numbers in `docs/bench.json`): the same data served by
this version, the first version (Bend + C) and its Next.js/React
baseline; one worker each on one core, the load generator on the other;
browser metrics in headless Chrome at 4x CPU throttling (a phone),
medians of 5. After the feed cache and the query-plan fix:

| Page | This version | First version | Next.js |
|---|---|---|---|
| Home: requests/s, CPU per request | 8,456, 110 us | 8,608, 140 us | 118 |
| Blog page | 8,352, 105 us | 8,544, 135 us | 148 |
| Post page (no comments) | 8,292 | 8,185 | 238 |
| Post page, 20 comments | 5,237 | not measured | not measured |
| Author page | 8,557, 110 us | 8,250, 140 us | 153 |
| HTML, home (gzipped) | 6.5 KB (1.1) | 16.8 KB (3.3) | 47.4 KB (5.7) |
| JavaScript on reading pages (gzipped) | 35.8 KB (14.3) | 35.8 KB (14.3) | 583 KB (171) |
| First paint, home (phone speed) | 208 ms | 224 ms | 252 ms |
| Main-thread script, home | 19 ms | 33 ms | 117 ms |

Requests a second are bound by connection handling at this rate (each
request is a new TCP connection in the load generator); the CPU per
request is the better comparison between the two native servers.

- **A 6 MB novel**: its page is rendered once (about 200 ms) and then
  served from memory: 247 requests a second of 7.4 MB each; first paint
  248 ms at phone speed. The first version could not hold a post this
  long (1 MiB cap).
- **Substack** (a real page, over the internet, same throttling):
  116 KB of HTML, 6.4 MB of JavaScript (2 MB gzipped) in 157 files,
  first paint 1,032-1,376 ms, 2.4 s of main-thread script. Ours: first
  paint about 200 ms and 19 ms of script. Network time is not comparable
  (theirs crossed the internet, ours did not); the script and bytes are.
- **Editor**: 15 ms a key at 1M and 3M characters, 11-18 ms in a 300 KB
  paragraph; 17 MB of memory (JS heap and WebAssembly) at 1M characters.
- **What made the difference, measured**: feed pages cached by a
  database generation (225 to 110 us); SQLite re-planning statements
  with a bound LIMIT on every request (fixed with the query planner
  stability guarantee: 3,450 to 5,237 requests a second on a comment
  page).
