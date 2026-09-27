// Every limit, named, in one place, with the worst case it allows for and
// why (DESIGN.md, "Limits and the 10x rule"). Tests go to ten times the
// worst case, and past each limit (a clear refusal).
use vstd::prelude::*;

verus! {

/// A request's head (request line and headers). Browsers send 1-4 KB;
/// the largest real heads are cookie-heavy, ~8 KB. Past it: 431.
pub const HEAD_MAX: usize = 16 * 1024;

/// Headers in one request. Browsers send ~15; proxies add a few. Past it: 431.
pub const HEADERS_MAX: usize = 64;

/// A request's target (path and query). Our URLs are short; search
/// queries are at most 200 characters, percent-encoded ~600. Past it: 414.
pub const TARGET_MAX: usize = 4096;

/// Open connections per worker. A viral post: ~10k readers at once; the
/// 10x test holds 100k. An idle connection costs a slot (~64 bytes), not
/// a buffer. Past it: new connections are closed at once.
pub const CONN_MAX: usize = 100_000;

/// Connections holding part of a head (split across packets, or sent
/// slowly): a small buffer each while the part is at most PARTIAL_SMALL
/// bytes, a HEAD_MAX one past that (and input read ahead of a response
/// still being sent). When a pool is empty, the connection that has been
/// receiving its head longest (at least EVICT_AGE_MS) is closed (408) and
/// its buffer given to the newcomer: a flood of slow senders cannot lock
/// readers out. With no such connection, the newcomer gets 503.
pub const PARTIAL_SMALL: usize = 2048;
pub const PARTIAL_SMALL_MAX: usize = 32_768;
pub const EVICT_AGE_MS: u64 = 1000;
pub const PARTIAL_MAX: usize = 1024;

/// Output a client has not taken yet (it reads slowly) is kept per
/// connection, all of it within PENDING_BYTES_MAX. Past it, that
/// connection is closed. (Large bodies will be sent from their stored
/// bytes, not copied per connection: DESIGN.md.)
pub const PENDING_MAX: usize = CONN_MAX;
pub const PENDING_BYTES_MAX: usize = 256 * 1024 * 1024;

/// A head must arrive within this (a client sending a byte a minute holds
/// nothing for long). Then: 408 and close.
pub const HEAD_TIMEOUT_MS: u64 = 10_000;

/// An idle keep-alive connection is closed after this.
pub const IDLE_TIMEOUT_MS: u64 = 60_000;

/// A parked request (one the application answers later: an editor
/// waiting for others' changes) is closed after this if still unanswered
/// (the application answers within PARK_ANSWER_MS).
pub const PARK_TIMEOUT_MS: u64 = 40_000;

/// While requests are parked, the loop asks the application this often
/// whether any can be answered.
pub const PARK_TICK_MS: u64 = 20;

/// Events handled per wait (the loop's batch).
pub const EVENTS_MAX: usize = 1024;

/// A request's body. The largest are sync uploads of a pasted manuscript,
/// sent in pieces of at most SYNC_BODY_MAX (src/doc/); forms are small.
/// Past it: 413.
pub const BODY_MAX: usize = 16 * 1024 * 1024;

/// All bodies being received at once, within this. Past it: 503.
pub const BODY_BYTES_MAX: usize = 512 * 1024 * 1024;

/// A body arrives at least this fast once past its first HEAD_TIMEOUT_MS
/// (bytes a second): a slow upload does not hold a buffer forever.
pub const BODY_MIN_RATE: u64 = 16 * 1024;

} // verus!
