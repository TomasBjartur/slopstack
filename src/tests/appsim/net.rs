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
    /// Bytes on the way, each with when it arrives.
    up: VecDeque<(u64, Vec<u8>)>,
    down: VecDeque<(u64, Vec<u8>)>,
    /// When each side's close reaches the other (u64::MAX: not closed).
    client_closed: u64,
    server_closed: u64,
    /// Ended without the server's close (refused, or its worker crashed).
    pub reset: bool,
    lat: u64,
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
}

impl Net {
    pub fn new(seed: u64) -> Net {
        Net { now: 0, skew: 0, rng: Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1), conns: vec![], backlog: VecDeque::new(), slots: map(), rough: true, digest: 0xcbf2_9ce4_8422_2325 }
    }

    pub fn wall(&self) -> u64 {
        (EPOCH as i64 + self.now as i64 + self.skew) as u64
    }

    /// A client connects and sends bytes (in pieces, after the latency).
    pub fn connect(&mut self, bytes: Vec<u8>) -> usize {
        let lat = 1 + if self.rough && self.rng.below(10) == 0 { self.rng.below(400) } else { self.rng.below(15) };
        let mut up = VecDeque::new();
        let mut at = 0;
        let mut t = self.now + lat;
        while at < bytes.len() {
            let n = if self.rough && self.rng.below(4) == 0 { 1 + self.rng.below((bytes.len() - at) as u64) as usize } else { bytes.len() - at };
            up.push_back((t, bytes[at..at + n].to_vec()));
            at += n;
            t += self.rng.below(3);
        }
        self.conns.push(Conn { worker: usize::MAX, slot: 0, up, down: VecDeque::new(), client_closed: u64::MAX, server_closed: u64::MAX, reset: false, lat });
        let c = self.conns.len() - 1;
        self.backlog.push_back(c);
        c
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
        let n = self.net.borrow();
        if !n.backlog.is_empty() {
            out.push(IoEvent { slot: LISTENER, read: true, write: false });
        }
        let mut mine: Vec<(u32, usize)> = n.slots.iter().filter(|((w, _), _)| *w == self.w).map(|((_, s), &c)| (*s, c)).collect();
        mine.sort_unstable();
        for (slot, c) in mine {
            let k = &n.conns[c];
            let readable = k.up.front().is_some_and(|(t, _)| *t <= n.now) || (k.up.is_empty() && k.client_closed <= n.now);
            out.push(IoEvent { slot, read: readable, write: true });
        }
    }

    fn accept(&mut self, slot: u32) -> bool {
        let mut n = self.net.borrow_mut();
        let now = n.now;
        // (Only connections whose first bytes could have arrived.)
        let Some(pos) = n.backlog.iter().position(|&c| n.conns[c].up.front().map_or(true, |(t, _)| *t <= now)) else { return false };
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
            Some((t, bytes)) if *t <= now => {
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
