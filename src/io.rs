// Everything the server does to the outside world goes through Io, so the
// simulator (src/sim.rs) can run the same server under a seeded model of
// the network and the clock. LinuxIo is production: epoll, edge-triggered.
use crate::limits::*;
use crate::sys::linux as sys;

/// A read: bytes, nothing now (wait for the next event), or the peer is gone.
#[derive(Debug, PartialEq)]
pub enum Rd {
    Data(usize),
    Later,
    Gone,
}

/// A write: bytes taken, no room now, or the peer is gone.
#[derive(Debug, PartialEq)]
pub enum Wr {
    Sent(usize),
    Later,
    Gone,
}

/// Something happened: on the listener (slot LISTENER) or a connection.
#[derive(Clone, Copy, Debug)]
pub struct IoEvent {
    pub slot: u32,
    pub read: bool,
    pub write: bool,
}

pub const LISTENER: u32 = u32::MAX;

pub trait Io {
    fn now_ms(&mut self) -> u64;
    fn random(&mut self, out: &mut [u8]);
    /// Waits at most timeout_ms for events; appends them to out.
    fn wait(&mut self, out: &mut Vec<IoEvent>, timeout_ms: u64);
    /// Takes one waiting connection as slot; false if none is waiting.
    fn accept(&mut self, slot: u32) -> bool;
    /// Takes one waiting connection and closes it (no slot free); false if
    /// none is waiting.
    fn refuse(&mut self) -> bool;
    fn read(&mut self, slot: u32, buf: &mut [u8]) -> Rd;
    fn write(&mut self, slot: u32, buf: &[u8]) -> Wr;
    fn close(&mut self, slot: u32);
}

pub struct LinuxIo {
    listener: sys::fd,
    ep: sys::Epoll,
    fds: Vec<sys::fd>,
    evs: Vec<sys::epoll_event>,
}

impl LinuxIo {
    pub fn new(port: u16) -> Result<LinuxIo, i32> {
        LinuxIo::on(sys::listen_loopback(port, 4096)?, false)
    }

    /// On a listening socket (made before the workers were forked: shared
    /// is set, and each connection wakes one worker).
    pub fn on(listener: sys::fd, shared: bool) -> Result<LinuxIo, i32> {
        let ep = sys::Epoll::new()?;
        let excl = if shared { sys::EPOLLEXCLUSIVE } else { 0 };
        ep.add(listener, sys::EPOLLIN | sys::EPOLLET | excl, LISTENER as u64)?;
        Ok(LinuxIo { listener, ep, fds: vec![-1; CONN_MAX], evs: vec![sys::epoll_event { events: 0, data: 0 }; EVENTS_MAX] })
    }
}

impl Io for LinuxIo {
    fn now_ms(&mut self) -> u64 {
        sys::now_ms()
    }

    fn random(&mut self, out: &mut [u8]) {
        sys::random(out)
    }

    fn wait(&mut self, out: &mut Vec<IoEvent>, timeout_ms: u64) {
        let n = self.ep.wait(&mut self.evs, timeout_ms.min(i32::MAX as u64) as i32);
        for e in &self.evs[..n] {
            let (bits, data) = (e.events, e.data);
            let gone = bits & (sys::EPOLLERR | sys::EPOLLHUP | sys::EPOLLRDHUP) != 0;
            out.push(IoEvent { slot: data as u32, read: bits & sys::EPOLLIN != 0 || gone, write: bits & sys::EPOLLOUT != 0 || gone });
        }
    }

    fn accept(&mut self, slot: u32) -> bool {
        match sys::accept(self.listener) {
            Ok(c) => {
                if self.ep.add(c, sys::EPOLLIN | sys::EPOLLOUT | sys::EPOLLRDHUP | sys::EPOLLET, slot as u64).is_err() {
                    sys::close_fd(c);
                    return false;
                }
                self.fds[slot as usize] = c;
                true
            }
            Err(_) => false,
        }
    }

    fn refuse(&mut self) -> bool {
        match sys::accept(self.listener) {
            Ok(c) => {
                sys::close_fd(c);
                true
            }
            Err(_) => false,
        }
    }

    fn read(&mut self, slot: u32, buf: &mut [u8]) -> Rd {
        match sys::read_some(self.fds[slot as usize], buf) {
            Ok(0) => Rd::Gone,
            Ok(n) => Rd::Data(n),
            Err(sys::EAGAIN) => Rd::Later,
            Err(_) => Rd::Gone,
        }
    }

    fn write(&mut self, slot: u32, buf: &[u8]) -> Wr {
        match sys::write_some(self.fds[slot as usize], buf) {
            Ok(n) => Wr::Sent(n),
            Err(sys::EAGAIN) => Wr::Later,
            Err(_) => Wr::Gone,
        }
    }

    fn close(&mut self, slot: u32) {
        let f = self.fds[slot as usize];
        if f >= 0 {
            self.ep.del(f);
            sys::close_fd(f);
            self.fds[slot as usize] = -1;
        }
    }
}
