# Design

A multi-user blogging platform (sign up with a passkey, blogs, posts in
Markdown, co-authors, a real-time collaborative editor that works offline,
public reading pages), rebuilt from what the first version (Bend 2 + C,
`bent-web`, see its `docs/FINDINGS.md`) taught us. Two aims:

1. **Much faster than Substack** on every measure a reader or writer feels
   (time to first byte, largest paint, input latency, JS shipped) and on
   server cost, with published numbers. **Every page feels like an SPA**:
   no interaction waits on a full page load, clicks answer at once
   (optimistic), navigation is instant.
2. **Correctness checked as hard as we can**: machine-checked proofs about
   the code that runs, and every other check where proofs do not reach.
   Most of the code is written by an AI; the laws are owned by people.

## What the first version taught us

- Proofs caught the class of bugs they were aimed at (authorization,
  sessions, markup safety). Most bugs lived elsewhere: timeouts, a limit
  in one layer and not another, units (code points vs UTF-16), undo
  semantics, the page jumping. They were caught by differential fuzzing
  against a proved reference, property tests, mutation tests, and tests
  that drive a real browser. All of those carry over.
- The proof language's runtime (Bend: strings as cons lists, one event
  loop) pushed the fast paths into C, so the trust boundary kept growing.
  Here the proved code is the code that runs, at native speed.
- Sizes nobody tested broke things: a 200k-character post could not be
  saved; a novel hit a cap in one layer but not another; a textarea costs
  ~28 ms a key per MB in it. Hence the rule below: **test at ten times the
  largest input we can imagine**.
- The browser's own work sets the floor for the editor, not our code: the
  text must never all be in the editing element.
- Most of the "app feel" came from the platform (speculation rules, view
  transitions, the back/forward cache, HTTP caching), not from JS.

## Stack

| Part | Choice | Why |
|---|---|---|
| Server | Rust (pinned 1.98.1), **no crates at run time** | native speed, data-oriented layouts, Verus |
| Proofs about the code | **Verus** (pinned; SMT, Z3 bundled) | proofs about the Rust that runs |
| Proofs about models | **Lean 4** (pinned 4.34.1) | CRDT convergence over all delivery orders |
| Bounded checks | Kani (the `unsafe` and FFI parts) | what Verus cannot see |
| Database | SQLite (vendored amalgamation, hand-written FFI) | WAL, one writer, many readers |
| Crypto | HACL* (vendored, formally verified C): SHA-256, P-256 | passkeys, tokens |
| TLS, HTTP/2 | Caddy in front | we do not implement TLS |
| Browser | plain JS modules, the CRDT as WebAssembly (the same Verus-checked Rust) | one proved CRDT on both sides |
| Interaction with the server | Datastar (vendored, pinned, CSP mode), on every page | fragments patched in, optimistic signals: forms, likes, dashboards, co-authors, search, live updates |
| Client-owned work | our own JS modules | the editor, undo, Vim, Visual mode, dialogs |

**Dependencies.** None at run time besides the two vendored C libraries
(pinned by hash, with a written reason). Everything else we write: the
event loop (epoll, through a few hand-declared syscalls), HTTP/1.1,
routing, templates, JSON for passkeys, Markdown, the sanitizer, the CRDT.
Tools (rustc, Verus, Z3, Lean, Kani, clang, Chrome for tests) are pinned
and are not shipped. A crate needs a written reason and `cargo vendor`;
none is planned.

## Shape of the server

- **Processes**: a supervisor opens the listening socket and forks
  `BLOG_WORKERS` processes; each runs one event loop (epoll, edge-triggered,
  fixed pools sized at start). They share only the database: every cache
  is keyed by what the database says (a post's update time, a feed
  generation bumped by triggers, the last stored batch of a document).
  Besides it, one shared-memory page of change counters per post (made
  before the fork, `src/notify.rs`): a worker bumps a post's after
  committing a change, and requests waiting for that post, on any worker,
  are answered.
- **Pushed changes (long polling)**: a request may be parked
  (`Ctx::park`): the loop keeps the connection, holding no buffer, and
  asks the application every 20 ms (`App::ready`) for answers, named by
  slot and generation so none reaches a later connection in the slot. The
  editor keeps one sync request waiting (`?wait=1`), and a post page one
  live-comments request; each is answered when its post changes or after
  25 s, with the reader's rights decided again. Chosen over server-sent
  events: an ordinary request and answer, through any proxy, with no
  second protocol in the loop.
- **Work on the loop**: every request's work runs on its worker's loop,
  bounded by the limits and a query deadline; rendering a novel (about
  200 ms, then cached) is the longest. (Planned and not built: helper
  threads for data-sized work. Other workers keep serving meanwhile; if
  one worker's pauses matter, this is the next step.)
- **All I/O behind a trait** (`Io`: accept, read, write, close, timers,
  clock, randomness, the database). Production implements it with
  syscalls; the **simulator** implements it with a seeded model of the
  network, the clock and crashes, and runs the whole server in one thread,
  deterministically, under faults (drops, reordering, partial writes,
  crashes, clock skew).
- **Memory**: fixed pools allocated at start from named limits
  (connections, buffers, rows). Past a limit the answer is a clear refusal
  (503, 413, 429), never an allocation.
- **Pages** are written straight into the response buffer by template
  functions (no render plans: those were for the first version's slow
  strings). A post's published Markdown is rendered when read (hundreds of
  MB a second: a typical post in well under a millisecond, a novel in tens
  of milliseconds); a page cache keyed by (page, generation) serves repeat
  reads with no work. No rendered HTML is stored: one source of truth.

## Feeling like an SPA

Every page is served whole (fast first paint, works without JS), then
behaves like an app:
- **Navigation**: speculation rules prerender links on hover (the next page
  is ready before the click); view transitions cross-fade; the
  back/forward cache keeps pages alive. No client router.
- **Server interaction through Datastar** (one small cached script on every
  page): a form or button sends with fetch and the server answers with
  fragments to patch in (never a whole page); signals update the page at
  once (a like counts before the server answers) and the answer confirms
  it. Live updates (co-authors' edits to titles, new comments later)
  arrive over the same channel.
- **Local JS where the browser must own the work**: the editor (local-first,
  collaborative), undo, Vim, Visual mode, dialogs.
- **Tested like the rest**: browser tests measure input-to-next-paint for
  every interaction (budget: one frame for local work, 100 ms for a server
  round trip on the same machine), and fail on a full page load where a
  fragment was expected, on layout shift, or on a flash of unstyled or
  stale content.

## Documents (novel-length from the start)

- **A post's published text** is its Markdown, stored when it is published
  or updated; reading pages render it (and cache the page). The working
  text is the CRDT's.
- **The CRDT is Fugue** (as before), stored and sent in **runs**: text
  typed or pasted in one go by one writer is one record (id range, parent,
  side, UTF-8 text); deletions are id ranges. A pasted novel is one run,
  not millions of operations (the old wire format was ~40 bytes a
  character). The wire format is binary.
- **Snapshots**: the server keeps the document's merged state (runs and
  tombstone ranges) and the operations since; a client loads the snapshot
  and the tail. The limit is on the state's size, not on how many edits a
  post has ever had.
- **The browser holds the text in blocks** (not one string, for the
  browser's layout cost, not ours): the view
  (a textarea window over the text, static text elsewhere; as in the first
  version, which works) may cut inside a paragraph, so a 1.5 MB paragraph
  types as fast as short ones.

## Correctness: the layers

1. **Laws** (`spec/`, human-owned; a change is a SECURITY DECISION in the
   commit message): authorization, sessions, markup safety, parser bounds
   and limits, write budgets, CRDT merge laws. Stated with their own
   predicates, never the code's; each one mutation-tested (a deliberately
   broken version must fail it).
2. **Verus proofs** that the code meets the laws; **Lean** for the CRDT
   model (convergence under any delivery order); **Kani** for `unsafe`.
3. **Differential tests**: every fast implementation against a slow,
   obviously correct reference (the CRDT runs against the Lean model's
   executable version).
4. **Property tests and fuzzing** with our own seeded generators.
5. **Deterministic simulation** of the whole server under faults.
6. **Browser tests** that act as people do (keyboard, mouse, two writers,
   offline, input methods, a phone, a throttled CPU).
7. **Performance budgets as tests**, at ten times the worst case (below).
8. **Adversarial review**: a separate pass that attacks the code and the
   laws.

Every guarantee is labelled "proved about the code", "proved about a
model", or "tested (how)".

## Principles learned in the building

- **The 10x rule counts the browser's work too.** No cost per keystroke may
  grow with the document, ours or the browser's: a textarea lays its text
  out as one unit, an IntersectionObserver checks every target it has,
  `content-visibility: auto` watches every element that has it. Three of
  the novel-length slowdowns were browser work we had triggered.
- **Every cache names its key, and the key comes from the database**: an
  id that is never reused, or a version the database issues (updated_ms,
  the feed generation, a batch's seq). A key the database can reuse leaked
  one post's text into another's.
- **Stored time is wall-clock; timeouts are monotonic** (`Io::wall_ms`,
  `Io::now_ms`). Mixing them gave dates in 1970 and sessions that did not
  expire.
- **Pinned dependencies get tests of the behaviour we rely on.** Datastar
  1.0.4 sends no second request to a URL from the same element, and leaves
  an indicator set when an answer replaces the form that sent it; both are
  now tested, so an upgrade or a wrong belief fails loudly.
- **Usability rules are tested rules**: no dead ends (every error offers the
  next step: logged out while writing, "Log in"); after logging in, back
  where you were; every format turns off the way it turned on; a
  preference set is kept.
- *Tentative*: **keep the JavaScript on the proved path small.** The
  renderer and the CRDT are Rust (proved, or checked against a proved
  model) in the browser as WebAssembly; the editor's own JavaScript
  (Visual mode, Vim, paste, the view) is only tested, and growing. Before
  more logic goes there, ask whether it belongs in the WebAssembly.

## Limits and the 10x rule

Every input has a named limit in `src/limits.rs`, with the worst case we
can imagine a real user producing. Performance tests run at **ten times
that**, with the same budgets; correctness tests go past the limit and
require a clear refusal. Tests also measure growth: at N and 10N the time
must grow as the algorithm says (a 10x input taking 100x is a failure even
if it is fast). Browser tests run with the CPU throttled (a phone).

| Input | Worst case we imagine | Tested at | Why that worst case |
|---|---|---|---|
| A post's text | 6 MB (a 1M-word novel) | 60 MB | the longest novels |
| One paragraph | 150 KB | 1.5 MB | Molly Bloom's soliloquy, ~130 KB |
| One paste | 6 MB | 60 MB | a whole manuscript |
| A post's edit history | 10x its text | 100x | years of revision |
| Writers in one post at once | 20 | 200 | a newsroom |
| Posts in a blog | 100k | 1M | 50 a day for five years |
| Comments on a post | 50k | 500k | a viral post |
| Requests a second (cached) | 5k | 50k | the front page of everything |
| Open connections | 10k | 100k | readers of a viral post |
| Blogs per account | 100 | 1000 | a publisher |

Budgets (first version, to be tightened): a key to the next frame within
one frame on a desktop and two on a phone-speed CPU; a reading page's
first byte from cache under 1 ms of server CPU; a post opens for editing
within 2 s at the worst case (10x: within 20 s, and typing at once).
