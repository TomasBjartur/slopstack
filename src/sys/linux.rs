// Linux system calls, declared by hand (no crates): the only `unsafe` in the
// server besides the SQLite binding. Each wrapper checks what the kernel
// can return and turns it into a Result; none keeps a pointer past the call.
#![allow(non_camel_case_types)]
use std::os::raw::{c_int, c_void};

pub type fd = c_int;

#[repr(C)]
struct sockaddr_in {
    sin_family: u16,
    sin_port: u16, // network order
    sin_addr: u32, // network order
    sin_zero: [u8; 8],
}

// x86_64: the kernel's struct is packed (12 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct epoll_event {
    pub events: u32,
    pub data: u64,
}

#[repr(C)]
struct timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

extern "C" {
    fn socket(domain: c_int, ty: c_int, proto: c_int) -> c_int;
    fn setsockopt(fd: c_int, level: c_int, name: c_int, val: *const c_void, len: u32) -> c_int;
    fn bind(fd: c_int, addr: *const sockaddr_in, len: u32) -> c_int;
    fn listen(fd: c_int, backlog: c_int) -> c_int;
    fn accept4(fd: c_int, addr: *mut c_void, len: *mut u32, flags: c_int) -> c_int;
    fn read(fd: c_int, buf: *mut c_void, n: usize) -> isize;
    fn write(fd: c_int, buf: *const c_void, n: usize) -> isize;
    fn close(fd: c_int) -> c_int;
    fn epoll_create1(flags: c_int) -> c_int;
    fn epoll_ctl(ep: c_int, op: c_int, fd: c_int, ev: *mut epoll_event) -> c_int;
    fn epoll_wait(ep: c_int, evs: *mut epoll_event, max: c_int, timeout: c_int) -> c_int;
    fn clock_gettime(clk: c_int, ts: *mut timespec) -> c_int;
    fn getrandom(buf: *mut c_void, n: usize, flags: u32) -> isize;
    fn fork() -> c_int;
    fn waitpid(pid: c_int, status: *mut c_int, options: c_int) -> c_int;
    fn prctl(option: c_int, arg2: u64, arg3: u64, arg4: u64, arg5: u64) -> c_int;
    fn getppid() -> c_int;
    fn __errno_location() -> *mut c_int;
}

const AF_INET: c_int = 2;
const SOCK_STREAM: c_int = 1;
const SOCK_NONBLOCK: c_int = 0o4000;
const SOCK_CLOEXEC: c_int = 0o2000000;
const SOL_SOCKET: c_int = 1;
const SO_REUSEADDR: c_int = 2;
const IPPROTO_TCP: c_int = 6;
const TCP_NODELAY: c_int = 1;
pub const EPOLLIN: u32 = 0x1;
pub const EPOLLOUT: u32 = 0x4;
pub const EPOLLERR: u32 = 0x8;
pub const EPOLLHUP: u32 = 0x10;
pub const EPOLLRDHUP: u32 = 0x2000;
pub const EPOLLET: u32 = 1 << 31;
/// Of several epolls waiting on one listener, wake one (worker processes).
pub const EPOLLEXCLUSIVE: u32 = 1 << 28;
const PR_SET_PDEATHSIG: c_int = 1;
const SIGTERM: u64 = 15;
const EPOLL_CTL_ADD: c_int = 1;
const EPOLL_CTL_DEL: c_int = 2;
const EPOLL_CLOEXEC: c_int = 0o2000000;
const CLOCK_REALTIME: c_int = 0;
const CLOCK_MONOTONIC: c_int = 1;
pub const EAGAIN: c_int = 11;
const EINTR: c_int = 4;

fn errno() -> c_int {
    // SAFETY: __errno_location returns this thread's errno slot, always valid.
    unsafe { *__errno_location() }
}

/// A listening TCP socket on 127.0.0.1:port (Caddy is in front), non-blocking.
pub fn listen_loopback(port: u16, backlog: i32) -> Result<fd, c_int> {
    // SAFETY: plain syscalls with values and pointers to locals that outlive them.
    unsafe {
        let s = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK | SOCK_CLOEXEC, 0);
        if s < 0 {
            return Err(errno());
        }
        let one: c_int = 1;
        setsockopt(s, SOL_SOCKET, SO_REUSEADDR, &one as *const c_int as *const c_void, 4);
        let addr = sockaddr_in { sin_family: AF_INET as u16, sin_port: port.to_be(), sin_addr: u32::from_be_bytes([127, 0, 0, 1]).to_be(), sin_zero: [0; 8] };
        if bind(s, &addr, std::mem::size_of::<sockaddr_in>() as u32) < 0 || listen(s, backlog) < 0 {
            let e = errno();
            close(s);
            return Err(e);
        }
        Ok(s)
    }
}

// PROCESSES (worker processes: src/main.rs)
/// fork(2): Ok(0) in the child, Ok(pid) in the parent.
pub fn fork_process() -> Result<i32, c_int> {
    // SAFETY: fork has no pointer arguments. The caller forks before
    // opening anything that must not be shared (the database is opened in
    // each child, after the fork) and before starting threads (there are
    // none).
    let pid = unsafe { fork() };
    if pid < 0 { Err(errno()) } else { Ok(pid) }
}

/// Waits for any child to end: its pid.
pub fn wait_child() -> Result<i32, c_int> {
    let mut status: c_int = 0;
    loop {
        // SAFETY: status is a local that outlives the call.
        let pid = unsafe { waitpid(-1, &mut status, 0) };
        if pid >= 0 {
            return Ok(pid);
        }
        let e = errno();
        if e != EINTR {
            return Err(e);
        }
    }
}

/// This process gets SIGTERM when its parent ends (and ends now if the
/// parent already has).
pub fn die_with_parent(parent: i32) {
    // SAFETY: prctl with integer arguments only; getppid has none.
    unsafe {
        prctl(PR_SET_PDEATHSIG, SIGTERM, 0, 0, 0);
        if getppid() != parent {
            std::process::exit(0);
        }
    }
}

pub fn parent_pid() -> i32 {
    // SAFETY: no arguments.
    unsafe { getppid() }
}

/// The next connection, non-blocking, with Nagle off; Err(EAGAIN) if none.
pub fn accept(listener: fd) -> Result<fd, c_int> {
    // SAFETY: a null address is allowed (we do not need the peer's).
    unsafe {
        let c = accept4(listener, std::ptr::null_mut(), std::ptr::null_mut(), SOCK_NONBLOCK | SOCK_CLOEXEC);
        if c < 0 {
            return Err(errno());
        }
        let one: c_int = 1;
        setsockopt(c, IPPROTO_TCP, TCP_NODELAY, &one as *const c_int as *const c_void, 4);
        Ok(c)
    }
}

/// Reads into buf: Ok(n) (0: the peer closed), Err(EAGAIN) if nothing now.
pub fn read_some(f: fd, buf: &mut [u8]) -> Result<usize, c_int> {
    loop {
        // SAFETY: buf is valid for buf.len() bytes for the call.
        let n = unsafe { read(f, buf.as_mut_ptr() as *mut c_void, buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = errno();
        if e != EINTR {
            return Err(e);
        }
    }
}

/// Writes from buf: Ok(n) (n <= buf.len()), Err(EAGAIN) if the socket is full.
pub fn write_some(f: fd, buf: &[u8]) -> Result<usize, c_int> {
    loop {
        // SAFETY: buf is valid for buf.len() bytes for the call.
        let n = unsafe { write(f, buf.as_ptr() as *const c_void, buf.len()) };
        if n >= 0 {
            return Ok(n as usize);
        }
        let e = errno();
        if e != EINTR {
            return Err(e);
        }
    }
}

pub fn close_fd(f: fd) {
    // SAFETY: closing a descriptor we own; errors are ignored (nothing to do).
    unsafe {
        close(f);
    }
}

pub struct Epoll(fd);

impl Epoll {
    pub fn new() -> Result<Epoll, c_int> {
        // SAFETY: plain syscall.
        let e = unsafe { epoll_create1(EPOLL_CLOEXEC) };
        if e < 0 { Err(errno()) } else { Ok(Epoll(e)) }
    }

    pub fn add(&self, f: fd, events: u32, data: u64) -> Result<(), c_int> {
        let mut ev = epoll_event { events, data };
        // SAFETY: ev is a valid event for the call.
        if unsafe { epoll_ctl(self.0, EPOLL_CTL_ADD, f, &mut ev) } < 0 { Err(errno()) } else { Ok(()) }
    }

    pub fn del(&self, f: fd) {
        let mut ev = epoll_event { events: 0, data: 0 };
        // SAFETY: as above (a non-null event, for old kernels).
        unsafe {
            epoll_ctl(self.0, EPOLL_CTL_DEL, f, &mut ev);
        }
    }

    /// Waits up to timeout_ms (-1: no limit) and fills out; answers how many.
    pub fn wait(&self, out: &mut [epoll_event], timeout_ms: i32) -> usize {
        // SAFETY: out is valid for out.len() events; the kernel writes at most that many.
        let n = unsafe { epoll_wait(self.0, out.as_mut_ptr(), out.len() as c_int, timeout_ms) };
        if n < 0 { 0 } else { n as usize }
    }
}

/// Monotonic milliseconds (since boot): for timeouts and deadlines only.
pub fn now_ms() -> u64 {
    clock_ms(CLOCK_MONOTONIC)
}

/// Wall-clock milliseconds since 1970: for what is stored (dates, expiry).
pub fn wall_ms() -> u64 {
    clock_ms(CLOCK_REALTIME)
}

fn clock_ms(clock: c_int) -> u64 {
    let mut ts = timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: ts is a valid timespec for the call.
    unsafe {
        clock_gettime(clock, &mut ts);
    }
    ts.tv_sec.max(0) as u64 * 1000 + ts.tv_nsec.max(0) as u64 / 1_000_000
}

/// Fills buf from the kernel's random source (blocks only before the
/// kernel's pool is ready, at boot).
pub fn random(buf: &mut [u8]) {
    let mut at = 0;
    while at < buf.len() {
        // SAFETY: the rest of buf is valid for the call.
        let n = unsafe { getrandom(buf[at..].as_mut_ptr() as *mut c_void, buf.len() - at, 0) };
        if n > 0 {
            at += n as usize;
        } else if errno() != EINTR {
            panic!("getrandom failed");
        }
    }
}
