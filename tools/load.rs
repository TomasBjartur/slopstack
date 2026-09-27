// A load generator (std only): C keep-alive connections over T threads,
// each sending GET <path> and reading the answer, for S seconds. Prints
// requests a second and latency percentiles.
// usage: load <port> <path> <connections> <threads> <seconds>
use std::io::{Read, Write};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let port: u16 = a[1].parse().unwrap();
    let path = a[2].clone();
    let conns: usize = a[3].parse().unwrap();
    let threads: usize = a[4].parse().unwrap();
    let secs: u64 = a[5].parse().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let mut hs = vec![];
    for t in 0..threads {
        let stop = stop.clone();
        let path = path.clone();
        let mine = conns / threads + if t < conns % threads { 1 } else { 0 };
        hs.push(std::thread::spawn(move || {
            let req = format!("GET {path} HTTP/1.1\r\nHost: x\r\n\r\n").into_bytes();
            let mut socks: Vec<TcpStream> = (0..mine)
                .map(|_| {
                    let s = TcpStream::connect(("127.0.0.1", port)).unwrap();
                    s.set_nodelay(true).unwrap();
                    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
                    s
                })
                .collect();
            let mut lat: Vec<u32> = Vec::with_capacity(1 << 20);
            let mut buf = vec![0u8; 1 << 16];
            while !stop.load(Ordering::Relaxed) {
                // One request on every connection, then every answer.
                let t0 = Instant::now();
                for s in &mut socks {
                    s.write_all(&req).unwrap();
                }
                for s in &mut socks {
                    read_answer(s, &mut buf);
                    lat.push(t0.elapsed().as_micros() as u32);
                }
            }
            lat
        }));
    }
    let t0 = Instant::now();
    std::thread::sleep(Duration::from_secs(secs));
    stop.store(true, Ordering::Relaxed);
    let mut all: Vec<u32> = hs.into_iter().flat_map(|h| h.join().unwrap()).collect();
    let el = t0.elapsed().as_secs_f64();
    all.sort_unstable();
    let p = |q: f64| all[((all.len() as f64 * q) as usize).min(all.len() - 1)];
    println!(
        "{} requests in {el:.1} s: {:.0} a second; latency p50 {} us, p99 {} us, max {} us",
        all.len(),
        all.len() as f64 / el,
        p(0.5),
        p(0.99),
        all[all.len() - 1]
    );
}

/// Reads one answer: the head, then Content-Length bytes.
fn read_answer(s: &mut TcpStream, buf: &mut [u8]) {
    let mut have = 0;
    loop {
        let n = s.read(&mut buf[have..]).unwrap();
        assert!(n > 0, "closed");
        have += n;
        if let Some(e) = buf[..have].windows(4).position(|w| w == b"\r\n\r\n") {
            let head = std::str::from_utf8(&buf[..e]).unwrap();
            let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().trim().parse().unwrap();
            let mut need = e + 4 + len;
            while have < need {
                let cap = buf.len();
                let n = s.read(&mut buf[have..need.min(cap)]).unwrap();
                assert!(n > 0);
                have += n;
                if have == buf.len() {
                    need -= have;
                    have = 0;
                }
            }
            return;
        }
    }
}
