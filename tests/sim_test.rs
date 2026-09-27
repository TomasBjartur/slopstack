// The server (src/server.rs) under the simulated network (src/sim.rs):
// many seeds, each a crowd of clients (normal, pipelining, slow readers,
// slow senders, broken requests, vanishing, and a flood). Checks:
//   - every client that sent whole valid requests and stayed got exactly
//     one answer per request, in order (the echo says which);
//   - a broken head gets its error, then the connection closes;
//   - a sender slower than HEAD_TIMEOUT gets 408;
//   - with more half-sent heads than PARTIAL_MAX, the rest get 503 and
//     nothing breaks;
//   - after the idle timeout, every connection is closed and every buffer
//     is back in its pool (nothing leaks);
//   - the same seed gives the same run (deterministic).
// 10x: one seed runs 100,000 connections at once (CONN_MAX: ten times the
// worst crowd we expect).
use vstd::prelude::*;

#[path = "../src/limits.rs"]
pub mod limits;
#[path = "../spec/http.rs"]
pub mod spec_http;
#[path = "../src/http.rs"]
pub mod http;
#[path = "../src/sys/linux.rs"]
pub mod sys_linux;
pub mod sys {
    pub use super::sys_linux as linux;
}
#[path = "../src/io.rs"]
pub mod io;
#[path = "../src/server.rs"]
pub mod server;
#[path = "../src/app.rs"]
pub mod app;
#[path = "../src/sim.rs"]
pub mod sim;

use limits::*;
use sim::{Rng, SimIo};

verus! {
#[verifier::external_body]
fn main() {
    run();
}
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum Kind {
    Normal,
    Pipelined,
    SlowReader,
    SlowSender,
    Broken,
    Vanishes,
    Flood,
}

/// The answers in a client's bytes: (status, body) each.
fn answers(b: &[u8]) -> Vec<(u16, Vec<u8>)> {
    let mut out = vec![];
    let mut p = 0;
    while p < b.len() {
        let Some(e) = find(&b[p..], b"\r\n\r\n") else { break };
        let head = std::str::from_utf8(&b[p..p + e]).unwrap_or("");
        let status: u16 = head.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0);
        let len: usize = head
            .lines()
            .find_map(|l| l.strip_prefix("Content-Length: "))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let start = p + e + 4;
        if start + len > b.len() {
            break;
        }
        out.push((status, b[start..start + len].to_vec()));
        p = start + len;
    }
    out
}

fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

struct Plan {
    kind: Kind,
    ids: Vec<String>,
}

/// A crowd of clients under one seed. split: heads may arrive in any
/// pieces (adversarial); else most arrive whole, as over a real network.
fn run_seed(seed: u64, crowd: usize, split: bool, check: &mut dyn FnMut(bool, String)) -> (u64, u64, u64) {
    let mut io = SimIo::new(seed);
    io.split_reads = split;
    let mut rng = Rng(seed.wrapping_mul(0x9e37) | 1);
    let mut plans = vec![];
    for c in 0..crowd {
        let kind = match rng.below(20) {
            0..=7 => Kind::Normal,
            8..=10 => Kind::Pipelined,
            11..=12 => Kind::SlowReader,
            13 => Kind::SlowSender,
            14..=15 => Kind::Broken,
            16..=17 => Kind::Vanishes,
            _ => Kind::Flood,
        };
        let n = match kind {
            Kind::Pipelined => 2 + rng.below(6) as usize,
            Kind::Normal | Kind::SlowReader => 1 + rng.below(3) as usize,
            _ => 1,
        };
        let ids: Vec<String> = (0..n).map(|k| format!("c{c}r{k}")).collect();
        let mut bytes = vec![];
        for id in &ids {
            bytes.extend_from_slice(format!("GET /echo/{id} HTTP/1.1\r\nHost: sim\r\nX-Pad: {}\r\n\r\n", "p".repeat(rng.below(400) as usize)).as_bytes());
        }
        match kind {
            Kind::Broken => bytes = b"GET /echo/x HTTP/1.1\r\nBad Header\r\n\r\n".to_vec(),
            Kind::Flood => bytes = b"GET /echo/never-finished HTTP/1.1\r\nX-Slow: ".to_vec(),
            _ => {}
        }
        let (window, reads, pace, vanish) = match kind {
            Kind::SlowReader => (64, 16, 5, u64::MAX),
            Kind::SlowSender => (65536, 65536, HEAD_TIMEOUT_MS + 2000, u64::MAX),
            Kind::Vanishes => (65536, 65536, 50, 5 + rng.below(100)),
            Kind::Flood => (65536, 65536, 10, u64::MAX),
            _ => (65536, 65536, 20, u64::MAX),
        };
        let piece = if kind == Kind::SlowSender { 1 } else if split || rng.below(20) == 0 { usize::MAX } else { 1 << 20 };
        io.connect(bytes, window, reads, pace, vanish, piece);
        plans.push(Plan { kind, ids });
    }
    let mut s = server::Server::new(io, app::Site);
    // Run past the head and idle timeouts, so every connection ends.
    while s.io.now < IDLE_TIMEOUT_MS + HEAD_TIMEOUT_MS + 5000 {
        s.turn(50);
    }
    let floods = plans.iter().filter(|p| p.kind == Kind::Flood).count();
    for (i, p) in plans.iter().enumerate() {
        let c = &s.io.clients[i];
        let got = answers(&c.got);
        match p.kind {
            Kind::Normal | Kind::Pipelined | Kind::SlowReader => {
                let want: Vec<(u16, Vec<u8>)> = p.ids.iter().map(|id| (200, id.as_bytes().to_vec())).collect();
                check(got == want, format!("seed {seed} client {i} ({:?}): answers {:?}", p.kind, got.iter().map(|(s, b)| (*s, String::from_utf8_lossy(b).to_string())).collect::<Vec<_>>()));
            }
            Kind::Broken => check(got.len() == 1 && got[0].0 == 400 && c.server_closed, format!("seed {seed} client {i}: a broken head: {:?}", got.iter().map(|a| a.0).collect::<Vec<_>>())),
            Kind::SlowSender => check(got.len() == 1 && got[0].0 == 408 && c.server_closed, format!("seed {seed} client {i}: too slow a sender: {:?}", got.iter().map(|a| a.0).collect::<Vec<_>>())),
            Kind::Flood => check(got.len() == 1 && (got[0].0 == 408 || got[0].0 == 503) && c.server_closed, format!("seed {seed} client {i}: flood: {:?}", got.iter().map(|a| a.0).collect::<Vec<_>>())),
            Kind::Vanishes => {}
        }
    }
    check(s.open_connections() == 0, format!("seed {seed}: {} connections still open after the timeouts", s.open_connections()));
    check(s.buffers_in_use() == (0, 0), format!("seed {seed}: buffers still taken: {:?}", s.buffers_in_use()));
    let _ = floods;
    (s.stats.requests, s.stats.accepted, s.stats.refused)
}

fn flood_then_readers(check: &mut dyn FnMut(bool, String)) {
    let mut io = SimIo::new(4242);
    let flood = PARTIAL_SMALL_MAX + PARTIAL_MAX + 2000;
    for _ in 0..flood {
        io.connect(b"GET /echo/never HTTP/1.1\r\nX-Slow: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_vec(), 65536, 65536, 900, u64::MAX, 8);
    }
    let mut s = server::Server::new(io, app::Site);
    while s.io.now < 2000 {
        s.turn(50);
    }
    let first = s.io.clients.len();
    for k in 0..200 {
        s.io.connect(format!("GET /echo/r{k} HTTP/1.1\r\nHost: sim\r\n\r\n").into_bytes(), 65536, 65536, 20, u64::MAX, usize::MAX);
    }
    while s.io.now < 6000 {
        s.turn(50);
    }
    let mut ok = 0;
    for k in 0..200 {
        let got = answers(&s.io.clients[first + k].got);
        if got == vec![(200, format!("r{k}").into_bytes())] {
            ok += 1;
        }
    }
    check(ok == 200, format!("after a flood of {flood} slow senders, {ok} of 200 readers were answered"));
    println!("a flood of {flood} slow senders, then 200 readers: {ok} answered; {} flood connections evicted", s.stats.evicted);
}

fn run() {
    let mut fails = 0;
    let mut checks = 0;
    let mut check = |ok: bool, msg: String| {
        checks += 1;
        if !ok {
            fails += 1;
            if fails <= 10 {
                println!("FAIL {msg}");
            }
        }
    };
    let t = std::time::Instant::now();
    let mut total = 0;
    for seed in 1..=200u64 {
        let (req, _, _) = run_seed(seed, 60, true, &mut check);
        total += req;
    }
    println!("200 seeds of 60 clients: {total} requests answered ({:.1} s)", t.elapsed().as_secs_f64());
    // Deterministic: the same seed, the same run.
    let a = run_seed(77, 60, true, &mut |_, _| {});
    let b = run_seed(77, 60, true, &mut |_, _| {});
    check(a == b, format!("the same seed gave different runs: {a:?} {b:?}"));
    // A flood of slow senders, more than the head buffers, then readers:
    // the readers are all answered (the flood's oldest are evicted).
    flood_then_readers(&mut check);
    // 10x: 100,000 connections at once.
    let t = std::time::Instant::now();
    let mut big_fails = std::collections::BTreeMap::new();
    let (req, acc, _) = run_seed(99, CONN_MAX, false, &mut |ok, msg| {
        if !ok {
            if std::env::var("SIM_VERBOSE").is_ok() {
                println!("  {msg}");
            }
            let k = msg.split(':').nth(1).unwrap_or("").chars().take(30).collect::<String>();
            *big_fails.entry(k).or_insert(0) += 1;
        }
    });
    for (k, n) in &big_fails {
        check(false, format!("100k run: {n} x{k}"));
    }
    println!("100,000 connections at once: {acc} accepted, {req} requests answered ({:.1} s simulated run)", t.elapsed().as_secs_f64());
    println!("\n{checks} checks, {fails} failure(s)");
    std::process::exit(if fails > 0 { 1 } else { 0 });
}
