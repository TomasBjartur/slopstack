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
| A parked request's answer reaches only its own connection; a parked client that leaves or sends more is closed; every parked request ends | `src/server.rs` (`answer`, generations) | tested in the simulator (20 seeds of 300 clients, slots reused by other parked requests; both guards mutation-checked) |
| A waiting request is answered with the post only if the asker may still see it (edit it, for the editor) when the answer is made | `src/site.rs` `answer_waits` | tested (`tests/push_test.py`: removed while waiting gets 403; unpublished, 404; mutation-checked) |

What is trusted and not proved: SQLite and HACL* (vendored; HACL* is
itself formally verified), the Rust compiler and standard library,
`src/sys/` (hand-declared syscalls and FFI, the only `unsafe`), the
browser, Caddy.

## Tests (all pass at the time of writing)

| Suite | What | Size |
|---|---|---|
| `build/server test http` | parser: property tests, 10x heads | growth checked N vs 10N |
| `build/server test sim` | the event loop under a seeded network: splits, slow readers and senders, floods, 100,000 connections, parked requests | 19,941 checks |
| `build/server test markdown` | CommonMark 0.31.2 | 576 exact, 76 deviations by law (raw HTML shown as text; unsafe schemes), 0 failures |
| `build/server test crdt` | the CRDT against the model, bad input, 10x budgets | 410 histories |
| `tests/crdt_lean.sh` | Rust against the Lean model's definitions | 200 histories |
| `tests/wasm_test.mjs` | the WebAssembly build against the server's | 652 + 100 renders |
| `tests/app_test.py` | flows and attacks over HTTP | 115 |
| `tests/passkey_test.py` | WebAuthn with a software authenticator | 71 |
| `tests/sync_test.py` | the editor's sync protocol and its attacks | 29 |
| `tests/push_test.py` | waits answered by changes on any of 3 workers, not by one's own; timeouts; limits; rights re-checked; live comments | 14 |
| `tests/redteam_test.py` | every kind of user against every route | 235 |
| `tests/browser_test.py` | Chrome with a virtual authenticator: sign-up, login, recovery, writing | 31 |
| `tests/collab_test.py` | two Chromes, two users, one document, offline | 16 |
| `tests/editor_test.py` | Vim, Visual mode, round trips | 32 |
| `tests/large_test.py` | 1M and 6M characters in the editor | 50 |
| `tests/text_test.mjs` | the editor's chunked text against plain strings | 3,000 random edits, every method |
| `VIM_TEXT=1 tests/vim_test.mjs` | Vim on the chunked text, cut every 3 units | the whole Vim suite |
| `tests/workers_test.py` | 4 worker processes: freshness, documents, supervision | 7 |
| `tests/fuzz_server.py` | mutated requests at a live server | 80,000 requests |
| `build/server test appsim` | the whole application simulated: 3 workers on one database, 12 users with passkeys, faults (network, clock jumps, database errors, crashes), the oracle on every answer, convergence at the end | 50 seeds of 2 simulated minutes, about 1,500 answers checked each (18 s) |
| `tools/sim_acceptance.py` | known bugs and breaks of the law put back one at a time: the simulation must find each | 8 of 8, each within 4 s of running |

## Bugs, and what found them

| Bug | Found by | Would a proof have caught it? |
|---|---|---|
| HACL*'s `uncompressed_to_raw` does not check the point is on the curve | a unit test | no: it is trusted code |
| Strict JSON accepted a repeated key (`"type"` twice in clientDataJSON) | the old passkey attack test | no: parsing is tested, not proved |
| Attestation statements other than `none` were ignored, not refused | the old passkey attack test | no (same) |
| The application stored monotonic time (ms since boot): dates in 1970, expired sessions still valid | the old red-team test, ported | no: the laws are about who, not when. The simulator could not see it (its clock is fake either way) |
| Deleting a blog left its posts' documents in memory; SQLite reused the post id; a new post by someone else showed the old text | the red-team test | no: authorization was right (the new owner may read their post); the cache was keyed by a reusable id. Fixed at the root: ids are never reused |
| Statement ids offset by the migration's own statement | the first end-to-end run | — |
| An open redirect after logging in: the page's check of `?next=` accepted `/\host`, which browsers read as `//host` | the new login test (Chrome), trying hostile destinations | no: the page's JavaScript is not proved; the server's own check already refused it. Both now refuse backslashes |
| Reopened in Visual mode, every block was read-only ("A long block: edit it in Markdown mode"): the mode opened before the WebAssembly renderer had loaded | using the editor in Chrome (a usability pass) | no |
| Visual mode flattened nested lists when writing Markdown back (a sub-item's text joined its parent's) | adding Tab to nest list items | no |
| Datastar 1.0.4 sends no second request to the same URL from the same element (replies, likes, a second co-author did nothing) | the comments test in two Chromes | no |
| The first edit at a new place in a pasted novel with accents read the run from its start (a paste is one run): 30 ms in the browser at 6M characters, 300 ms at 60M | profiling the editor at 6M; the CRDT's own budget test had used ASCII text, whose byte offsets need no reading | no: a cost, not a wrong answer. Runs are now capped at 4,096 characters (split as an edit would split them); a test times the slowest of 200 edits at new places (27 us; 10 ms uncapped) |
| Not sending a writer its own changes back skipped by the replica that sent the request; a reopened page sends edits kept from an earlier page load under that page's number, so it was never sent that page's saved edits (and showed nothing) | the usability test in Chrome (offline edits, browser shut down, reopened) | no: the rule was a claim about the client ("it has them") that was false in one case. Now the page names its own replica (`?me=`), which is new each page load; a test sends an earlier page's edits and checks it gets that page's stored ones (mutation-checked) |
| A failed COMMIT (busy, full, I/O) left its transaction open: that worker held the write lock, and every other worker's writes failed from then on (in production: a busy COMMIT, and the site stops taking edits until a restart) | the whole-app simulation, on its second run (an injected COMMIT error; the invariant "no transaction open between requests") | no: the proofs are about who may write, not about a write that fails. Fixed: `Store::commit` rolls back on failure |
| A sync batch whose transaction failed after it had been applied to the worker's copy of the document stayed in the copy: the client's retry looked like a repeat and was not stored, and its later batches, which build on it, were; the stored document then had operations whose parents it lacked, and failed to load everywhere else | the whole-app simulation (a client's CRDT refused an answer: "Missing"; then a post that answered 503 after the faults stopped) | no. Fixed: a failed write that stored operations makes the worker forget its copy |
| `tools/check.sh` could not fail on a failing test: each was piped into `tail` (the shell has no pipefail), and the CRDT suite never exited non-zero | adding the simulation to it | no. Each test's exit status now decides |
| A comment could show twice: the answer to your own comment and the live answer cross (rare with polling, every time once live comments were pushed) | the comments test in Chrome, after push | no. The page keeps the first of each comment id |
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
- **Rendering a novel stays on the event loop (decided, measured).** A
  6 MB post holds its worker 67-84 ms while it renders (a small page on
  the same worker waited that long), once per version. Moving it to a
  helper thread would cost a way to handle a request again later and a
  thread pool; not worth one sub-100 ms pause per update. What was worth
  it: a draft's page and its Preview rendered the novel again on every
  view (130-165 ms each); they are now cached by the document's seq
  (39-42 ms, mostly sending 6 MB; tested that an edit shows at once,
  mutation-checked).
- **Allocations on the request path: left as they are (measured).** Under
  load (one worker, 16 connections), `perf` puts allocation and copying
  (malloc, free, realloc, memcpy) at 0.5% of the server's time on the
  home page, 0.8% on a post page and 1.8% on a post with 20 comments; the
  kernel's networking and SQLite take most of the rest. Removing them
  would gain at most about 2%.
- **The CRDT's proof is close to trivial.** `lean/Fugue.lean` defines the
  text from the sorted set of elements; convergence and the merge laws
  then say that a function of a set gives the same answer for the same
  set. It would accept a CRDT that puts every character in the wrong
  place, or drops some (the walk silently skips an element whose parent
  is missing). What users rely on (an edit lands where it was made;
  concurrent runs do not interleave; every character appears once) is
  tested, not proved. Label: proved about a model, of little power.
- **The simulator covers the event loop, not the application.** It found
  three bugs no other test could reach (a slowloris locking readers out
  of head buffers; evicting a connection losing its output; memory at
  100,000 connections) and made parked requests safe to add (both guards
  mutation-checked). But it runs a small echo application: none of the
  application's bugs (dates in monotonic time, a reused id, a writer not
  sent an earlier page's edits) could appear in it.
- **Thesis 2, so far.** Proofs found no bugs; the bugs were around them.
  What found bugs: the simulator, real browsers, differential tests, the
  red team. The whole-app simulation found two in its first hour of
  running, in code (transactions, a cache) no law reaches. What the proofs gave that tests could not: the markup law for
  any input, and design pressure towards types that make wrong states
  unwritable (the most useful effect). "Most of a real web app can carry
  proofs" is not what we found; a small core can, and should, and the
  rest is best served by deterministic simulation (DESIGN.md).
- **The whole-app simulation: what it cost and what it found.** About
  2,200 lines of test code (network, clients with a passkey
  authenticator, users, the oracle, its reading of the law; 2,300 with
  the acceptance script) and about 180 lines of hooks in the server
  (keyed hash maps, a fault hook at `Store`, SQLite switches, signing
  for the simulated authenticator). Its first runs found two real bugs
  that every other test had missed (above), and it finds each bug put back
  (the table) within four seconds of running. Before it, the fast checks
  (unit tests and the event loop's simulator) found one of the eight (the
  stale answer to a reused connection); the others had been found by slow
  suites in Chrome or by the red team, by the proofs (the law's breaks),
  or not at all.

  | Bug put back | Simulation: first seed, simulated time, what saw it | The fast checks before |
  |---|---|---|
  | Dates in monotonic time | seed 1 at 0.9 s: a stored date before 2020 | not caught |
  | Reused post ids | seed 4 at 35 s: text of post p0 served as post 1, another post | not caught |
  | A writer not sent an earlier page's edits (`?me=`) | seed 1 at 78 s: a client's CRDT refused an answer | not caught |
  | A stale answer to a reused connection | seed 1 at 56 s: a client's CRDT refused another post's operations | caught (the event loop's simulator) |
  | Drafts readable by anyone (the law broken in `src/authz.rs`, as a build without proofs could) | seed 2 at 7 s: draft text to a reader the law excludes | not caught |
  | Anyone signed in may publish anyone's post (the same) | seed 1 at 34 s: a change the law does not permit | not caught |
  | A failed COMMIT left open | seed 11 at 97 s: a transaction open between requests | not caught |
  | A rolled-back batch kept in a worker's copy | seed 5 at the end: an editor not converged with the server | not caught |

  All eight within four seconds of running (`tools/sim_acceptance.py`,
  about 15 minutes with the builds).
- **But measured by mutation, it is weak (2026-09-28).** 60 random
  mutants of the server's code (`tools/mutants.py 60 1`: a comparison
  flipped, && and ||, true and false, an off-by-one, a statement
  removed): the simulation kills 12 (20%), the fast checks before it 17
  (28%), both together 21 (35%). Perhaps 8-10 survivors change nothing
  that matters; most are blind spots:
  - it checks that nothing forbidden happens, never that what is allowed
    works: a server that refuses everything passes. Survivors: every
    login refused; the registration challenge missing; every request
    with a cookie answered 400; nobody allowed to edit any post. Users
    saw refusals, which the oracle allows;
  - its clients are lenient (a JSON quote removed; a browser's parser
    would fail, the simulated one read on);
  - every simulated request closes its connection: keep-alive, which
    browsers use, never runs;
  - no invariant that a local edit does what it says (deletes made to do
    nothing, consistently everywhere, kept every replica converged), that
    feeds show new posts, that paging is right, or that every buffer is
    free at the end (the event loop's simulator caught two leaks it
    missed);
  - features it never uses: images, the form without JavaScript,
    recovery, likes, replies, "more comments", snapshots (16 survivors
    on lines it never runs; 76% of the server's lines run).
  "Catches all eight known bugs" said less than it seemed: it was built
  knowing them.
- **The simulation's own mistakes, before it was right**: four false
  alarms (two writers' tokens alike; the template's "[deleted]"
  placeholder taken for rendered text; random deletions splicing two
  tokens into what read as a third; its report slicing a string inside
  a character, which ended a run silently) and one real hang (SQLite
  sleeping 5 s per statement behind the open transaction, before busy
  locks were made to answer at once in the simulation). Each was fixed in
  the harness, not the server.
- **A law must be tested from the wrong side.** At first the simulated
  users only did what they were allowed to: a draft leak put into the
  authorization code went unseen in 100 seeds. Curious users (one action
  in twelve tries others' things) found it in the first seed. And the
  oracle first judged with the server's own `authorize`: a bug there, in
  a build without proofs, would blind it to the very leak it made. It
  now has its own reading of the law (`law.rs`, proved equal to the spec
  separately; a wrong copy fails verification).
- **What it cannot see**: the DOM and the browser (a comment shown twice
  was found in Chrome), Datastar, layout, input methods, Safari, and
  `web/editor.js` itself (its clients follow the editor's rules; they are
  not the editor). Two catches are indirect: the rolled-back batch was
  seen only as editors never settling, and a stale answer as a CRDT
  refusing operations, not by a rule naming them. The markup oracle is
  an independent reading of the law, tested, not proved; the
  authorization oracle is proved equal to the law, on facts the oracle
  reads with its own SQL. Seeded hash maps turned out
  precautionary: in these runs no cache grew past its budget, so their
  order never mattered; the replay check would show if it did.
- **Reading a profile wrong.** Totals over ten keys were read as costs
  per key (27 ms of WebAssembly "a key" was one 27 ms first edit). The
  fix it pointed at was still right, but the claimed gain was not; timing
  single edits showed it. Profiles are now compared per key.
- **Visual mode still takes the whole text on a remote change or undo**
  (`String(view.text)`, then split into blocks): O(n) at such moments,
  not per local key.
- **Real-time was polling (fixed): now pushed.** The editor asked every
  0.4 s (1.5 s when idle) and waited 150 ms after the last key before
  sending, so a co-author saw nothing while someone typed steadily
  (3.2 s of typing, measured: nothing until the pause). Now a request
  waits at the server for others' changes (long polling; parked in the
  event loop), woken across worker processes through shared counters;
  edits are sent every 100 ms while typing. Measured in two Chromes:

  | From A's key to B's screen | Polling | Pushed |
  |---|---|---|
  | After a quiet spell | median 883 ms, worst 1,064 | 107 ms, worst 109 |
  | Typing a key a second | median 476 ms, worst 530 | 107 ms, worst 108 |
  | Typing a key every 50 ms | nothing for 3.2 s | first key after 109 ms, then about 55 ms behind |

  (The 100 ms is A's own send interval; the push itself takes about
  7 ms.) Live comments are pushed the same way (they were polled every
  10 s). A writer is no longer sent its own changes back.
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
- **Editor**: 15 ms a key at 1M and 6M characters (a frame), 11-18 ms in
  a 300 KB paragraph; 17 MB of memory (JS heap and WebAssembly) at 1M
  characters. At 6M: typing 15-16 ms, Vim `x` 27 ms, typing in Visual
  mode 25 ms (before the chunked text: 16-29, 30 and 33 ms).
- **The editor's text is chunked** (`web/text.js`: 8 KB chunks and their
  starts, a value like a string). As one JavaScript string, every edit to
  a novel made a new 12 MB string, flattened by the next search or slice,
  and garbage: about 6 ms a key at 6M. Now an edit copies the chunk it
  touches and a table of about 730 starts: 16 us. Vim reads text through
  string methods only (`charAt`, not `t[i]`), so it takes either.
- **What a key costs now, at 6M (Vim `x`)**: the textarea's own layout of
  its window when the text is set from script (`setRangeText`, about
  9 ms) and measuring the caret's place (about 4 ms). Neither grows with
  the document, only with the window (about 25-50 KB); typing, which the
  browser inserts itself, does not pay the first.
- **What made the difference, measured**: feed pages cached by a
  database generation (225 to 110 us); SQLite re-planning statements
  with a bound LIMIT on every request (fixed with the query planner
  stability guarantee: 3,450 to 5,237 requests a second on a comment
  page).
