// The network of the whole-application simulation: clients connect to one
// listening socket shared by several workers (as the worker processes
// share it), bytes arrive after a latency, in pieces, and connections end
// by either side, by a refusal, or by their worker crashing. Time is the
// simulation's: the harness moves it; the wall clock may jump apart from
// the monotonic one (Net::skew).
use crate::hash::{map, Map};
use crate::io::{Io, IoEvent, Rd, Wr, LISTENER};
use crate::sim::Rng;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

/// The wall clock at simulated time 0 (2026).
pub const EPOCH: u64 = 1_790_000_000_000;

pub struct Conn {
    /// The worker that accepted it (usize::MAX: waiting to be accepted).
    pub worker: usize,
    pub slot: u32,
    /// Bytes on the way, each with when it arrives (and a number: which
    /// have been reported to the server, edge-triggered).
    up: VecDeque<(u64, Vec<u8>, u64)>,
    sent: u64,
    reported: u64,
    close_reported: bool,
    down: VecDeque<(u64, Vec<u8>)>,
    /// When each side's close reaches the other (u64::MAX: not closed).
    client_closed: u64,
    server_closed: u64,
    /// Ended without the server's close (refused, or its worker crashed).
    pub reset: bool,
    lat: u64,
    /// The last request line sent on it, and what the client did with it
    /// last (for reports).
    pub last: String,
    pub state: &'static str,
}

pub struct Net {
    /// Monotonic milliseconds.
    pub now: u64,
    /// The wall clock is EPOCH + now + skew.
    pub skew: i64,
    pub rng: Rng,
    pub conns: Vec<Conn>,
    backlog: VecDeque<usize>,
    /// (worker, slot) -> connection.
    slots: Map<(usize, u32), usize>,
    /// Faults on: pieces, slow links.
    pub rough: bool,
    /// Everything clients have received, folded into one number (a run's
    /// fingerprint: the same seed must give the same one).
    pub digest: u64,
    /// Idle kept-alive connections by user (as a browser keeps them), with
    /// when each fell idle; and which users' browsers keep connections.
    idle: Map<usize, Vec<(usize, u64)>>,
    pub keeps: Vec<bool>,
}

impl Net {
    pub fn new(seed: u64) -> Net {
        Net { now: 0, skew: 0, rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1), conns: vec![], backlog: VecDeque::new(), slots: map(), rough: true, digest: 0xcbf2_9ce4_8422_2325, idle: map(), keeps: vec![] }
    }

    pub fn wall(&self) -> u64 {
        (EPOCH as i64 + self.now as i64 + self.skew) as u64
    }

    /// A client connects and sends bytes (in pieces, after the latency).
    pub fn connect(&mut self, bytes: Vec<u8>) -> usize {
        let lat = 1 + if self.rough && self.rng.below(10) == 0 { self.rng.below(400) } else { self.rng.below(15) };
        self.conns.push(Conn { worker: usize::MAX, slot: 0, up: VecDeque::new(), sent: 0, reported: 0, close_reported: false, down: VecDeque::new(), client_closed: u64::MAX, server_closed: u64::MAX, reset: false, lat, last: String::new(), state: "sent" });
        let c = self.conns.len() - 1;
        self.put(c, &bytes);
        self.backlog.push_back(c);
        c
    }

    /// Bytes from the client on a connection, in pieces, after its latency.
    fn put(&mut self, c: usize, bytes: &[u8]) {
        let lat = self.conns[c].lat;
        self.conns[c].last = String::from_utf8_lossy(&bytes[..bytes.iter().position(|&b| b == b'\r').unwrap_or(bytes.len())]).to_string();
        let mut at = 0;
        let mut t = self.now + lat;
        while at < bytes.len() {
            let n = if self.rough && self.rng.below(4) == 0 { 1 + self.rng.below((bytes.len() - at) as u64) as usize } else { bytes.len() - at };
            self.conns[c].sent += 1;
            let seq = self.conns[c].sent;
            self.conns[c].up.push_back((t, bytes[at..at + n].to_vec(), seq));
            at += n;
            t += self.rng.below(3);
        }
    }

    /// A request from a user: on a kept connection of theirs if one is
    /// idle and still open (as a browser reuses them), else a new one.
    /// Answers the connection and whether it may be kept after.
    pub fn request(&mut self, user: usize, bytes: Vec<u8>, keep: bool) -> usize {
        if keep {
            let now = self.now;
            let mut pool = self.idle.remove(&user).unwrap_or_default();
            // (A browser drops what has been idle long: the server's own limit is 60 s.)
            for &(c, at) in pool.iter().filter(|(_, at)| now - at > 50_000) {
                let _ = at;
                self.client_close(c);
            }
            pool.retain(|(_, at)| now - at <= 50_000);
            while let Some((c, _)) = pool.pop() {
                if self.open(c) {
                    self.idle.insert(user, pool);
                    self.conns[c].state = "sent again";
                    self.put(c, &bytes);
                    return c;
                }
            }
            self.idle.insert(user, pool);
        }
        self.connect(bytes)
    }

    /// A kept connection, idle again.
    pub fn release(&mut self, user: usize, c: usize) {
        self.conns[c].state = "released";
        if self.open(c) {
            let now = self.now;
            self.idle.entry(user).or_default().push((c, now));
        }
    }

    /// Kept connections closed as browsers shut, but some browsers leave
    /// theirs open (then the server's idle timeout must close them).
    pub fn close_idle(&mut self) {
        let mut all: Vec<(usize, usize)> = self.idle.iter().flat_map(|(u, v)| v.iter().map(move |x| (*u, x.0))).collect();
        all.sort_unstable();
        self.idle.clear();
        for (u, c) in all {
            if u % 2 == 0 {
                self.client_close(c);
            }
        }
    }

    /// (Debugging: the state of a worker's connections still open.)
    pub fn describe(&self, w: usize) -> Vec<String> {
        let mut v: Vec<String> = self.slots.iter().filter(|((ww, _), _)| *ww == w).map(|((_, s), &c)| {
            let k = &self.conns[c];
            format!("slot {s} conn {c} ({}, {}): up {:?} down {} client_closed {} server_closed {} reset {}", k.last, k.state, k.up.iter().map(|x| (x.0, x.1.len())).collect::<Vec<_>>(), k.down.len(), k.client_closed, k.server_closed, k.reset)
        }).collect();
        v.sort();
        v
    }

    fn open(&self, c: usize) -> bool {
        let k = &self.conns[c];
        !k.reset && k.server_closed == u64::MAX && k.client_closed == u64::MAX
    }

    /// What has reached the client: the bytes, and whether the connection
    /// has ended (and whether by a reset).
    pub fn client_take(&mut self, c: usize) -> (Vec<u8>, bool, bool) {
        let now = self.now;
        let k = &mut self.conns[c];
        let mut got = vec![];
        while k.down.front().is_some_and(|(t, _)| *t <= now) {
            got.extend_from_slice(&k.down.pop_front().expect("front").1);
        }
        let ended = k.down.is_empty() && (k.server_closed <= now || k.reset);
        let reset = k.reset;
        for b in &got {
            self.digest = (self.digest ^ *b as u64).wrapping_mul(0x100_0000_01b3);
        }
        (got, ended, reset)
    }

    /// The client closes (gives up, goes offline, closes the page).
    pub fn client_close(&mut self, c: usize) {
        self.conns[c].state = "closed";
        let now = self.now;
        let k = &mut self.conns[c];
        if k.client_closed == u64::MAX {
            k.client_closed = now + k.lat;
        }
        if k.worker == usize::MAX {
            // (Not accepted yet: it goes away unseen.)
            k.reset = true;
            self.backlog.retain(|&x| x != c);
        }
    }

    /// A worker dies: its connections end, answered or not.
    pub fn crash_worker(&mut self, w: usize) {
        let gone: Vec<usize> = self.slots.iter().filter(|((ww, _), _)| *ww == w).map(|(_, &c)| c).collect();
        for c in gone {
            let k = &mut self.conns[c];
            k.reset = true;
            k.down.clear();
        }
        self.slots.retain(|(ww, _), _| *ww != w);
    }

    /// Connections with bytes or a close still on the way, or open: the
    /// simulation is quiet when there are none but idle ones.
    pub fn busy(&self) -> usize {
        self.conns.iter().filter(|k| !k.reset && k.server_closed == u64::MAX && k.client_closed == u64::MAX).count()
    }

    fn conn(&mut self, w: usize, slot: u32) -> Option<&mut Conn> {
        let c = *self.slots.get(&(w, slot))?;
        Some(&mut self.conns[c])
    }
}

/// One worker's view of the network (its Io).
pub struct WorkerIo {
    pub net: Rc<RefCell<Net>>,
    pub w: usize,
}

impl Io for WorkerIo {
    fn now_ms(&mut self) -> u64 {
        self.net.borrow().now
    }

    fn wall_ms(&mut self) -> u64 {
        self.net.borrow().wall()
    }

    fn random(&mut self, out: &mut [u8]) {
        let mut n = self.net.borrow_mut();
        for b in out {
            *b = n.rng.next() as u8;
        }
    }

    fn wait(&mut self, out: &mut Vec<IoEvent>, _timeout_ms: u64) {
        // (The harness moves time; a wait only reports what is ready now.)
        // Reads are EDGE-TRIGGERED, as the server asks epoll (EPOLLET): a
        // connection is reported readable once when new bytes (or its
        // close) arrive, not again while they sit unread. A server that
        // stops reading early is left waiting, as it would be.
        let mut n = self.net.borrow_mut();
        let now = n.now;
        if !n.backlog.is_empty() {
            out.push(IoEvent { slot: LISTENER, read: true, write: false });
        }
        let mut mine: Vec<(u32, usize)> = n.slots.iter().filter(|((w, _), _)| *w == self.w).map(|((_, s), &c)| (*s, c)).collect();
        mine.sort_unstable();
        for (slot, c) in mine {
            let k = &mut n.conns[c];
            let newest = k.up.iter().filter(|(t, _, _)| *t <= now).map(|x| x.2).max().unwrap_or(0);
            let mut readable = newest > k.reported;
            k.reported = k.reported.max(newest);
            if k.client_closed <= now && !k.close_reported {
                k.close_reported = true;
                readable = true;
            }
            out.push(IoEvent { slot, read: readable, write: true });
        }
    }

    fn accept(&mut self, slot: u32) -> bool {
        let mut n = self.net.borrow_mut();
        let now = n.now;
        // (Only connections whose first bytes could have arrived.)
        let Some(pos) = n.backlog.iter().position(|&c| n.conns[c].up.front().map_or(true, |(t, _, _)| *t <= now)) else { return false };
        let c = n.backlog.remove(pos).expect("found");
        n.conns[c].worker = self.w;
        n.conns[c].slot = slot;
        n.slots.insert((self.w, slot), c);
        true
    }

    fn refuse(&mut self) -> bool {
        let mut n = self.net.borrow_mut();
        let Some(c) = n.backlog.pop_front() else { return false };
        n.conns[c].reset = true;
        true
    }

    fn read(&mut self, slot: u32, buf: &mut [u8]) -> Rd {
        let mut n = self.net.borrow_mut();
        let now = n.now;
        let Some(k) = n.conn(self.w, slot) else { return Rd::Gone };
        match k.up.front_mut() {
            Some((t, bytes, _)) if *t <= now => {
                let m = bytes.len().min(buf.len());
                buf[..m].copy_from_slice(&bytes[..m]);
                bytes.drain(..m);
                if bytes.is_empty() {
                    k.up.pop_front();
                }
                Rd::Data(m)
            }
            Some(_) => Rd::Later,
            None if k.client_closed <= now => Rd::Gone,
            None => Rd::Later,
        }
    }

    fn write(&mut self, slot: u32, buf: &[u8]) -> Wr {
        let mut n = self.net.borrow_mut();
        let now = n.now;
        // (Sometimes only part goes: the server keeps the rest for later.)
        let part = if n.rough && n.rng.below(8) == 0 { 1 + n.rng.below(buf.len() as u64) as usize } else { buf.len() };
        let Some(k) = n.conn(self.w, slot) else { return Wr::Gone };
        if k.client_closed <= now {
            return Wr::Gone;
        }
        let t = now + k.lat;
        k.down.push_back((t, buf[..part].to_vec()));
        Wr::Sent(part)
    }

    fn close(&mut self, slot: u32) {
        let mut n = self.net.borrow_mut();
        let now = n.now;
        if let Some(c) = n.slots.remove(&(self.w, slot)) {
            let k = &mut n.conns[c];
            // (After the last bytes written.)
            k.server_closed = k.down.back().map_or(now, |(t, _)| *t).max(now) + 1;
        }
    }
}
