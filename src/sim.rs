// A simulated network and clock (deterministic: one seed decides
// everything), implementing Io, so the real server runs in a test.
// Clients are scripts: bytes to send (split anywhere, sent at random
// moments), how fast they read, and whether they vanish. The network has
// a small, random socket buffer per direction, so writes block and resume.
use crate::io::{Io, IoEvent, Rd, Wr, LISTENER};

pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545f4914f6cdd1d)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
}

/// One simulated client connection.
pub struct Client {
    /// Bytes still to send, and when the next piece goes.
    pub to_send: Vec<u8>,
    pub next_send_ms: u64,
    /// Bytes on the wire to the server (not yet read by it).
    pub up: Vec<u8>,
    /// Bytes the server wrote that the client has not read yet.
    pub down: Vec<u8>,
    /// Everything the client has read.
    pub got: Vec<u8>,
    /// The client's receive window (bytes), and how many bytes it reads a turn.
    pub window: usize,
    pub reads: usize,
    /// Vanishes at this time (u64::MAX: never).
    pub vanish_ms: u64,
    pub slot: u32,
    pub accepted: bool,
    pub server_closed: bool,
    pub client_closed: bool,
    /// About how long between pieces, and the largest piece.
    pub pace_ms: u64,
    pub max_piece: usize,
}

pub struct SimIo {
    pub now: u64,
    pub rng: Rng,
    pub clients: Vec<Client>,
    /// Connections waiting to be accepted (indexes into clients).
    pub backlog: Vec<usize>,
    /// slot -> client
    pub by_slot: Vec<usize>,
    pub refused: u64,
    /// Reads return any part of what arrived (adversarial), or all of it
    /// (a real network: a small head arrives in one piece).
    pub split_reads: bool,
}

impl SimIo {
    pub fn new(seed: u64) -> Self {
        SimIo { now: 0, rng: Rng(seed | 1), clients: vec![], backlog: vec![], by_slot: vec![usize::MAX; crate::limits::CONN_MAX], refused: 0, split_reads: true }
    }

    /// A client that connects now and sends `bytes` in random pieces, every
    /// pace_ms or so.
    pub fn connect(&mut self, bytes: Vec<u8>, window: usize, reads: usize, pace_ms: u64, vanish_ms: u64, max_piece: usize) -> usize {
        let i = self.clients.len();
        self.clients.push(Client {
            to_send: bytes,
            next_send_ms: self.now,
            up: vec![],
            down: vec![],
            got: vec![],
            window,
            reads,
            vanish_ms,
            slot: u32::MAX,
            accepted: false,
            server_closed: false,
            client_closed: false,
            pace_ms,
            max_piece,
        });
        self.backlog.push(i);
        i
    }

    /// Clients act: send a piece, read some of what arrived, maybe vanish.
    fn step_clients(&mut self) {
        for i in 0..self.clients.len() {
            let now = self.now;
            let c = &mut self.clients[i];
            if c.client_closed || c.server_closed {
                continue;
            }
            if now >= c.vanish_ms {
                c.client_closed = true;
                continue;
            }
            if c.accepted && !c.to_send.is_empty() && now >= c.next_send_ms {
                // Any piece (max_piece usize::MAX), or exactly max_piece bytes.
                let k = if c.max_piece == usize::MAX { 1 + self.rng.below(c.to_send.len() as u64) as usize } else { c.max_piece.min(c.to_send.len()) };
                let piece: Vec<u8> = c.to_send.drain(..k.min(c.to_send.len())).collect();
                c.up.extend_from_slice(&piece);
                c.next_send_ms = now + self.rng.below(c.pace_ms + 1);
            }
            let r = c.reads.min(c.down.len());
            let taken: Vec<u8> = c.down.drain(..r).collect();
            c.got.extend_from_slice(&taken);
        }
    }
}

impl Io for SimIo {
    fn now_ms(&mut self) -> u64 {
        self.now
    }

    fn random(&mut self, out: &mut [u8]) {
        for b in out {
            *b = self.rng.next() as u8;
        }
    }

    fn wait(&mut self, out: &mut Vec<IoEvent>, timeout_ms: u64) {
        // Time passes (at most timeout_ms, at least 1 ms), clients act, and
        // every connection with something to do is reported.
        self.now += 1 + self.rng.below(timeout_ms.min(20));
        self.step_clients();
        if !self.backlog.is_empty() {
            out.push(IoEvent { slot: LISTENER, read: true, write: false });
        }
        for c in &self.clients {
            if !c.accepted || c.server_closed {
                continue;
            }
            let readable = !c.up.is_empty() || c.client_closed;
            let writable = c.down.len() < c.window;
            // Edge-triggered delivery is modelled loosely: events may repeat
            // or be reported together; the server must cope with both.
            if readable || writable {
                out.push(IoEvent { slot: c.slot, read: readable, write: writable });
            }
        }
    }

    fn accept(&mut self, slot: u32) -> bool {
        let Some(i) = self.backlog.first().copied() else { return false };
        self.backlog.remove(0);
        let c = &mut self.clients[i];
        c.accepted = true;
        c.slot = slot;
        self.by_slot[slot as usize] = i;
        true
    }

    fn refuse(&mut self) -> bool {
        let Some(i) = self.backlog.first().copied() else { return false };
        self.backlog.remove(0);
        self.clients[i].server_closed = true;
        self.refused += 1;
        true
    }

    fn read(&mut self, slot: u32, buf: &mut [u8]) -> Rd {
        let c = &mut self.clients[self.by_slot[slot as usize]];
        if c.up.is_empty() {
            return if c.client_closed { Rd::Gone } else { Rd::Later };
        }
        // Any amount up to what is there (reads come in pieces).
        let most = c.up.len().min(buf.len());
        let n = if self.split_reads { 1 + self.rng.below(most as u64) as usize } else { most };
        buf[..n].copy_from_slice(&c.up[..n]);
        c.up.drain(..n);
        Rd::Data(n)
    }

    fn write(&mut self, slot: u32, buf: &[u8]) -> Wr {
        let c = &mut self.clients[self.by_slot[slot as usize]];
        if c.client_closed {
            return Wr::Gone;
        }
        let room = c.window.saturating_sub(c.down.len());
        if room == 0 {
            return Wr::Later;
        }
        let n = room.min(buf.len());
        c.down.extend_from_slice(&buf[..n]);
        Wr::Sent(n)
    }

    fn close(&mut self, slot: u32) {
        let i = self.by_slot[slot as usize];
        let c = &mut self.clients[i];
        // What was written reaches the client before the close.
        let rest: Vec<u8> = c.down.drain(..).collect();
        c.got.extend_from_slice(&rest);
        c.server_closed = true;
        self.by_slot[slot as usize] = usize::MAX;
    }
}
