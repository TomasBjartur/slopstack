// The event loop: connections, heads, responses, timeouts. Tested, not
// proved (tests/sim_test.rs runs it under a seeded model of the network:
// bytes split anywhere, slow readers, slow senders, clients that vanish,
// floods, 100,000 connections; its invariants are listed there). The
// parser it uses is proved (src/http.rs).
//
// Memory is fixed from the limits, allocated at start, as flat arrays:
// - CONN_MAX slots of a few words (an idle keep-alive connection holds no
//   buffer);
// - part of a head, while it arrives, in a small buffer (PARTIAL_SMALL
//   bytes, PARTIAL_SMALL_MAX of them) or, past that, a HEAD_MAX one
//   (PARTIAL_MAX of them). A full pool gives the buffer held longest to
//   the newcomer (its owner gets 408): slow senders cannot lock readers out;
// - output a client has not taken yet (it reads slowly), PENDING_MAX
//   buffers. None left: that connection is closed.
use crate::http::{parse, Parsed};
use crate::io::{Io, IoEvent, Rd, Wr, LISTENER};
use crate::limits::*;
use crate::spec_http::HeaderPos;

const NONE: u32 = u32::MAX;
const BIG: u32 = 1 << 31; // a head buffer id with this bit is in the big pool

/// A request, as the application sees it (spans into the head's bytes).
pub struct Request<'a> {
    pub head: &'a [u8],
    pub method: &'a [u8],
    pub target: &'a [u8],
    pub headers: &'a [HeaderPos],
}

impl<'a> Request<'a> {
    pub fn header(&self, name: &[u8]) -> Option<&'a [u8]> {
        for h in self.headers {
            if self.head[h.start..h.name_end].eq_ignore_ascii_case(name) {
                return Some(&self.head[h.vs..h.ve]);
            }
        }
        None
    }
}

/// What the server asks of the application: a whole response in out.
/// Answers whether the connection may stay open.
pub trait App {
    fn handle(&mut self, req: &Request, now_ms: u64, out: &mut Vec<u8>) -> bool;
}

#[derive(Clone, Copy)]
struct Slot {
    open: bool,
    /// When the current head began (or the last one was answered).
    since_ms: u64,
    /// A head buffer (NONE, a small one, or BIG | index) and its fill.
    head: u32,
    filled: u32,
    /// Output waiting (index into pending) and how much of it is sent.
    out: u32,
    sent: u32,
    /// Close once the output is sent.
    close_after: bool,
}

const FREE_SLOT: Slot = Slot { open: false, since_ms: 0, head: NONE, filled: 0, out: NONE, sent: 0, close_after: false };

pub struct Stats {
    pub accepted: u64,
    pub requests: u64,
    pub refused: u64,
    pub timed_out: u64,
    pub evicted: u64,
}

/// A head buffer's bytes (by id: small, or BIG | index).
fn held<'a>(small: &'a Pool, big: &'a Pool, id: u32) -> &'a [u8] {
    let (p, i) = if id & BIG != 0 { (big, (id & !BIG) as usize) } else { (small, id as usize) };
    &p.bytes[i * p.size..(i + 1) * p.size]
}

/// A pool of equal buffers in one array, each with its owner.
struct Pool {
    size: usize,
    bytes: Vec<u8>,
    owner: Vec<u32>,
    free: Vec<u32>,
}

impl Pool {
    fn new(size: usize, n: usize) -> Pool {
        Pool { size, bytes: vec![0u8; size * n], owner: vec![NONE; n], free: (0..n as u32).rev().collect() }
    }
    fn buf(&mut self, i: u32) -> &mut [u8] {
        let s = i as usize * self.size;
        &mut self.bytes[s..s + self.size]
    }
    fn release(&mut self, i: u32) {
        self.owner[i as usize] = NONE;
        self.free.push(i);
    }
    fn used(&self) -> usize {
        self.owner.len() - self.free.len()
    }
}

pub struct Server<I: Io, A: App> {
    pub io: I,
    pub app: A,
    slots: Vec<Slot>,
    free_slots: Vec<u32>,
    small: Pool,
    big: Pool,
    pending: Vec<Vec<u8>>,
    free_pending: Vec<u32>,
    scratch: Vec<u8>,
    resp: Vec<u8>,
    hs: Vec<HeaderPos>,
    events: Vec<IoEvent>,
    open: usize,
    pending_bytes: usize,
    next_sweep_ms: u64,
    pub stats: Stats,
}

impl<I: Io, A: App> Server<I, A> {
    pub fn new(io: I, app: A) -> Self {
        Server {
            io,
            app,
            slots: vec![FREE_SLOT; CONN_MAX],
            free_slots: (0..CONN_MAX as u32).rev().collect(),
            small: Pool::new(PARTIAL_SMALL, PARTIAL_SMALL_MAX),
            big: Pool::new(HEAD_MAX, PARTIAL_MAX),
            pending: (0..PENDING_MAX).map(|_| Vec::new()).collect(),
            free_pending: (0..PENDING_MAX as u32).rev().collect(),
            scratch: vec![0u8; HEAD_MAX],
            resp: Vec::with_capacity(64 * 1024),
            hs: Vec::with_capacity(HEADERS_MAX),
            events: Vec::with_capacity(EVENTS_MAX),
            open: 0,
            pending_bytes: 0,
            next_sweep_ms: 0,
            stats: Stats { accepted: 0, requests: 0, refused: 0, timed_out: 0, evicted: 0 },
        }
    }

    pub fn open_connections(&self) -> usize {
        self.open
    }

    /// Buffers in use (heads, outputs): for tests of the pools.
    pub fn buffers_in_use(&self) -> (usize, usize) {
        (self.small.used() + self.big.used(), PENDING_MAX - self.free_pending.len())
    }

    /// One turn: wait for events (at most wait_ms), handle them, and time
    /// out connections that are too slow.
    pub fn turn(&mut self, wait_ms: u64) {
        let mut evs = std::mem::take(&mut self.events);
        evs.clear();
        self.io.wait(&mut evs, wait_ms);
        for e in &evs {
            if e.slot == LISTENER {
                self.accept_all();
            } else if (e.slot as usize) < CONN_MAX && self.slots[e.slot as usize].open {
                if e.write {
                    self.flush(e.slot);
                }
                if e.read && self.slots[e.slot as usize].open {
                    self.readable(e.slot);
                }
            }
        }
        self.events = evs;
        let now = self.io.now_ms();
        if now >= self.next_sweep_ms {
            self.next_sweep_ms = now + 1000;
            self.sweep(now);
        }
    }

    fn accept_all(&mut self) {
        loop {
            let Some(slot) = self.free_slots.pop() else {
                // Full: every waiting connection is closed at once (not left
                // waiting: with edge-triggered events it would never be seen).
                while self.io.refuse() {
                    self.stats.refused += 1;
                }
                return;
            };
            if !self.io.accept(slot) {
                self.free_slots.push(slot);
                return;
            }
            let now = self.io.now_ms();
            self.slots[slot as usize] = Slot { open: true, since_ms: now, ..FREE_SLOT };
            self.open += 1;
            self.stats.accepted += 1;
        }
    }

    fn close(&mut self, slot: u32) {
        let s = self.slots[slot as usize];
        if !s.open {
            return;
        }
        self.drop_head(slot);
        if s.out != NONE {
            self.release_pending(s.out);
        }
        self.slots[slot as usize] = FREE_SLOT;
        self.io.close(slot);
        self.free_slots.push(slot);
        self.open -= 1;
    }

    // HEAD BUFFERS
    fn release_pending(&mut self, p: u32) {
        let buf = &mut self.pending[p as usize];
        self.pending_bytes -= buf.len();
        buf.clear();
        if buf.capacity() > 64 * 1024 {
            // (Not kept: one large response should not pin its memory.)
            *buf = Vec::new();
        }
        self.free_pending.push(p);
    }

    fn head_buf(&mut self, id: u32) -> &mut [u8] {
        if id & BIG != 0 { self.big.buf(id & !BIG) } else { self.small.buf(id) }
    }

    fn drop_head(&mut self, slot: u32) {
        let h = self.slots[slot as usize].head;
        if h == NONE {
            return;
        }
        if h & BIG != 0 { self.big.release(h & !BIG) } else { self.small.release(h) }
        self.slots[slot as usize].head = NONE;
        self.slots[slot as usize].filled = 0;
    }

    /// A buffer from pool big (or small) for slot: a free one, or the one
    /// of the connection receiving its head longest, if at least
    /// EVICT_AGE_MS (it gets 408). None: no buffer to be had.
    fn take(&mut self, slot: u32, big: bool) -> Option<u32> {
        let now = self.io.now_ms();
        let pool = if big { &mut self.big } else { &mut self.small };
        if let Some(i) = pool.free.pop() {
            pool.owner[i as usize] = slot;
            return Some(if big { i | BIG } else { i });
        }
        // Evict the oldest sender of a head (never one sending a response).
        let mut oldest = NONE;
        let mut oldest_ms = now.saturating_sub(EVICT_AGE_MS) + 1;
        for (i, &o) in pool.owner.iter().enumerate() {
            if o != NONE && o != slot && self.slots[o as usize].out == NONE && self.slots[o as usize].since_ms < oldest_ms {
                oldest_ms = self.slots[o as usize].since_ms;
                oldest = i as u32;
            }
        }
        if oldest == NONE {
            return None;
        }
        let victim = pool.owner[oldest as usize];
        self.stats.evicted += 1;
        self.fail(victim, 408);
        self.close(victim);
        let pool = if big { &mut self.big } else { &mut self.small };
        let i = pool.free.pop()?;
        pool.owner[i as usize] = slot;
        Some(if big { i | BIG } else { i })
    }

    /// Keeps input [0, len) (in scratch, or already in the slot's buffer) in
    /// a buffer that can hold at least need bytes. False: none to be had.
    fn keep(&mut self, slot: u32, len: usize, from_scratch: bool, need: usize) -> bool {
        let s = self.slots[slot as usize];
        let big = need > PARTIAL_SMALL;
        let has = s.head != NONE && (s.head & BIG != 0 || !big);
        if has && !from_scratch {
            self.slots[slot as usize].filled = len as u32;
            return true;
        }
        let Some(id) = self.take(slot, big) else { return false };
        if from_scratch {
            let dst = if id & BIG != 0 { self.big.buf(id & !BIG) } else { self.small.buf(id) };
            dst[..len].copy_from_slice(&self.scratch[..len]);
        } else {
            // From the small buffer into the big one.
            let old = s.head as usize * PARTIAL_SMALL;
            self.big.buf(id & !BIG)[..len].copy_from_slice(&self.small.bytes[old..old + len]);
            self.small.release(s.head);
        }
        let now = self.io.now_ms();
        let sl = &mut self.slots[slot as usize];
        if sl.head == NONE {
            sl.since_ms = now;
        }
        sl.head = id;
        sl.filled = len as u32;
        true
    }

    /// Reads what there is and answers every whole request in it, in order.
    fn readable(&mut self, slot: u32) {
        loop {
            let s = self.slots[slot as usize];
            if !s.open || s.out != NONE {
                // Still sending: read more once the output is out.
                return;
            }
            let (rd, len, from_scratch) = if s.head != NONE {
                let at = s.filled as usize;
                let cap = if s.head & BIG != 0 { HEAD_MAX } else { PARTIAL_SMALL };
                if at == cap && cap < HEAD_MAX {
                    // The small buffer is full: move to a big one.
                    if !self.keep(slot, at, false, HEAD_MAX) {
                        self.fail(slot, 503);
                        return;
                    }
                    continue;
                }
                if at == cap {
                    (Rd::Data(0), at, false)
                } else {
                    let id = s.head;
                    let buf = if id & BIG != 0 { self.big.buf(id & !BIG) } else { self.small.buf(id) };
                    let r = self.io.read(slot, &mut buf[at..cap]);
                    let n = if let Rd::Data(n) = r { n } else { 0 };
                    (r, at + n, false)
                }
            } else {
                let r = self.io.read(slot, &mut self.scratch[..]);
                let n = if let Rd::Data(n) = r { n } else { 0 };
                (r, n, true)
            };
            match rd {
                Rd::Gone => return self.close(slot),
                Rd::Later => return,
                Rd::Data(_) => {}
            }
            if !self.process(slot, len, from_scratch) {
                return;
            }
        }
    }

    /// Answers the whole requests in the input [0, len) (in scratch or the
    /// slot's buffer). Answers whether to go on reading.
    fn process(&mut self, slot: u32, mut len: usize, mut from_scratch: bool) -> bool {
        loop {
            let s = self.slots[slot as usize];
            let (parsed, method_end, target_end) = {
                let buf: &[u8] = if from_scratch { &self.scratch[..len] } else { &held(&self.small, &self.big, s.head)[..len] };
                match parse(buf, &mut self.hs) {
                    Parsed::Done { method_end, target_end, len: used, .. } => (Ok(used), method_end, target_end),
                    Parsed::More => (Err(0u16), 0, 0),
                    Parsed::Bad(c) => (Err(c), 0, 0),
                }
            };
            match parsed {
                Err(0) => {
                    if len == 0 {
                        if !from_scratch {
                            self.drop_head(slot);
                        }
                        return true;
                    }
                    // Part of a head: keep it.
                    if !self.keep(slot, len, from_scratch, len + 1) {
                        self.stats.refused += 1;
                        self.fail(slot, 503);
                        return false;
                    }
                    return true;
                }
                Err(code) => {
                    self.fail(slot, code);
                    return false;
                }
                Ok(used) => {
                    self.stats.requests += 1;
                    let now = self.io.now_ms();
                    let keep = {
                        let head: &[u8] = if from_scratch { &self.scratch[..used] } else { &held(&self.small, &self.big, s.head)[..used] };
                        let req = Request { head, method: &head[..method_end], target: &head[method_end + 1..target_end], headers: &self.hs };
                        let has_body = req.header(b"content-length").map_or(false, |v| v != b"0") || req.header(b"transfer-encoding").is_some();
                        self.resp.clear();
                        if has_body {
                            // Bodies: not yet (the spike serves GET and HEAD).
                            error(&mut self.resp, 413);
                            false
                        } else {
                            self.app.handle(&req, now, &mut self.resp)
                        }
                    };
                    // The rest of the input (pipelined requests) to the front.
                    let rest = len - used;
                    if from_scratch {
                        self.scratch.copy_within(used..len, 0);
                    } else {
                        let id = s.head;
                        self.head_buf(id).copy_within(used..len, 0);
                        self.slots[slot as usize].filled = rest as u32;
                    }
                    len = rest;
                    self.slots[slot as usize].since_ms = now;
                    self.slots[slot as usize].close_after = !keep;
                    if !self.send(slot) || !keep {
                        return false;
                    }
                    if self.slots[slot as usize].out != NONE {
                        // Output waits: keep unread input for later.
                        if rest > 0 && !self.keep(slot, rest, from_scratch, rest) {
                            self.close(slot);
                            return false;
                        }
                        if rest == 0 && !from_scratch {
                            self.drop_head(slot);
                        }
                        return false;
                    }
                    if rest == 0 {
                        if !from_scratch {
                            self.drop_head(slot);
                        }
                        return true;
                    }
                    let _ = &mut from_scratch;
                }
            }
        }
    }


    fn fail(&mut self, slot: u32, code: u16) {
        if self.slots[slot as usize].out != NONE {
            // (A response is still going out: no second one behind it.)
            return self.close(slot);
        }
        self.resp.clear();
        error(&mut self.resp, code);
        self.slots[slot as usize].close_after = true;
        self.send(slot);
    }

    /// Sends self.resp; what does not go now waits in a pending buffer.
    /// Answers whether the connection is still open.
    fn send(&mut self, slot: u32) -> bool {
        let mut at = 0;
        while at < self.resp.len() {
            match self.io.write(slot, &self.resp[at..]) {
                Wr::Sent(n) => at += n,
                Wr::Later => break,
                Wr::Gone => {
                    self.close(slot);
                    return false;
                }
            }
        }
        if at == self.resp.len() {
            if self.slots[slot as usize].close_after {
                self.close(slot);
                return false;
            }
            return true;
        }
        let rest = self.resp.len() - at;
        if self.pending_bytes + rest > PENDING_BYTES_MAX {
            self.close(slot);
            return false;
        }
        let Some(p) = self.free_pending.pop() else {
            self.close(slot);
            return false;
        };
        self.pending_bytes += rest;
        let buf = &mut self.pending[p as usize];
        buf.clear();
        buf.extend_from_slice(&self.resp[at..]);
        let s = &mut self.slots[slot as usize];
        s.out = p;
        s.sent = 0;
        true
    }

    /// The socket has room: send what waits; then read what waited.
    fn flush(&mut self, slot: u32) {
        let s = self.slots[slot as usize];
        if s.out == NONE {
            return;
        }
        let mut sent = s.sent as usize;
        loop {
            let buf = &self.pending[s.out as usize];
            if sent == buf.len() {
                break;
            }
            match self.io.write(slot, &buf[sent..]) {
                Wr::Sent(n) => sent += n,
                Wr::Later => {
                    self.slots[slot as usize].sent = sent as u32;
                    return;
                }
                Wr::Gone => return self.close(slot),
            }
        }
        self.release_pending(s.out);
        self.slots[slot as usize].out = NONE;
        if s.close_after {
            return self.close(slot);
        }
        // Input that waited behind the output.
        if s.head != NONE && s.filled > 0 {
            if !self.process(slot, s.filled as usize, false) {
                return;
            }
        }
        self.readable(slot);
    }

    /// Closes connections too slow to send a head, or idle too long.
    fn sweep(&mut self, now: u64) {
        for i in 0..CONN_MAX {
            let s = self.slots[i];
            if !s.open {
                continue;
            }
            let late = if s.head != NONE { now >= s.since_ms + HEAD_TIMEOUT_MS } else { now >= s.since_ms + IDLE_TIMEOUT_MS };
            if late {
                self.stats.timed_out += 1;
                if s.head != NONE && s.out == NONE {
                    self.fail(i as u32, 408);
                } else {
                    self.close(i as u32);
                }
            }
        }
    }
}

/// A whole error response.
pub fn error(out: &mut Vec<u8>, code: u16) {
    let reason: &[u8] = match code {
        400 => b"Bad Request",
        404 => b"Not Found",
        408 => b"Request Timeout",
        413 => b"Content Too Large",
        414 => b"URI Too Long",
        431 => b"Request Header Fields Too Large",
        501 => b"Not Implemented",
        503 => b"Service Unavailable",
        _ => b"Error",
    };
    let mut n = [0u8; 20];
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(crate::app::itoa(code as u64, &mut n));
    out.extend_from_slice(b" ");
    out.extend_from_slice(reason);
    out.extend_from_slice(b"\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: ");
    out.extend_from_slice(crate::app::itoa(reason.len() as u64 + 1, &mut n));
    out.extend_from_slice(b"\r\nConnection: close\r\n\r\n");
    out.extend_from_slice(reason);
    out.push(b'\n');
}
