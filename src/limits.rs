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

} // verus!
