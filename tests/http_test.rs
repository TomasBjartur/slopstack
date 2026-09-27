// The HTTP parser (src/http.rs), run:
// - against a reference written the obvious way (split at CRLF, check each
//   part), on random heads from the grammar and random corruptions of them:
//   both must accept the same heads, with the same positions, and refuse
//   the same ones;
// - fed byte by byte: "more" until the head is whole, then the same answer;
// - the 10x rule: the worst real head is ~8 KB (cookies); at 80 KB it is
//   refused with 431, at once; and time grows linearly with the head.
use vstd::prelude::*;

#[path = "../src/limits.rs"]
pub mod limits;
#[path = "../spec/http.rs"]
pub mod spec_http;
#[path = "../src/http.rs"]
pub mod http;

use http::{parse, Parsed};
use limits::*;
use spec_http::HeaderPos;

verus! {
#[verifier::external_body]
fn main() {
    run();
}
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
    fn pick<'a>(&mut self, xs: &'a [&'a str]) -> &'a str {
        xs[self.below(xs.len() as u64) as usize]
    }
}

// THE REFERENCE: what a head is, written for clarity, not speed.
#[derive(Debug, PartialEq)]
enum Ref {
    Done(usize, usize, usize, Vec<(usize, usize, usize, usize, usize)>, usize),
    More,
    Bad(u16),
}

fn is_tchar(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b)
}

fn reference(buf: &[u8]) -> Ref {
    let n = buf.len().min(HEAD_MAX);
    let s = &buf[..n];
    let short = || if buf.len() >= HEAD_MAX { Ref::Bad(431) } else { Ref::More };
    // The request line.
    let Some(sp1) = s.iter().position(|&b| !is_tchar(b)) else { return short() };
    if sp1 == 0 || s[sp1] != b' ' {
        return Ref::Bad(400);
    }
    if !matches!(&s[..sp1], b"GET" | b"POST" | b"HEAD") {
        return Ref::Bad(501);
    }
    let ts = sp1 + 1;
    let te = ts + s[ts..].iter().position(|&b| !(0x21..=0x7e).contains(&b)).unwrap_or(n - ts);
    if te - ts > TARGET_MAX {
        return Ref::Bad(414);
    }
    if te == n {
        return short();
    }
    if te == ts || s[ts] != b'/' {
        return Ref::Bad(400);
    }
    if n - te < 11 {
        return short();
    }
    if &s[te..te + 11] != b" HTTP/1.1\r\n" {
        return Ref::Bad(400);
    }
    let line_end = te + 9;
    let mut p = line_end + 2;
    let mut hs = vec![];
    loop {
        if n - p < 2 {
            return short();
        }
        if s[p] == b'\r' {
            return if s[p + 1] == b'\n' { Ref::Done(sp1, te, line_end, hs, p + 2) } else { Ref::Bad(400) };
        }
        if hs.len() == HEADERS_MAX {
            return Ref::Bad(431);
        }
        let ls = p;
        let Some(k) = s[p..].iter().position(|&b| !is_tchar(b)) else { return short() };
        let ne = p + k;
        if ne == ls || s[ne] != b':' {
            return Ref::Bad(400);
        }
        let mut vs = ne + 1;
        while vs < n && (s[vs] == b' ' || s[vs] == b'\t') {
            vs += 1;
        }
        let mut e = vs;
        while e < n && ((0x21..=0x7e).contains(&s[e]) || s[e] == b' ' || s[e] == b'\t') {
            e += 1;
        }
        if n - e < 2 {
            return short();
        }
        if &s[e..e + 2] != b"\r\n" {
            return Ref::Bad(400);
        }
        let mut ve = e;
        while ve > vs && (s[ve - 1] == b' ' || s[ve - 1] == b'\t') {
            ve -= 1;
        }
        hs.push((ls, ne, vs, ve, e));
        p = e + 2;
    }
}

fn ours(buf: &[u8], hs: &mut Vec<HeaderPos>) -> Ref {
    match parse(buf, hs) {
        Parsed::Done { method_end, target_end, line_end, len } => {
            Ref::Done(method_end, target_end, line_end, hs.iter().map(|h| (h.start, h.name_end, h.vs, h.ve, h.end)).collect(), len)
        }
        Parsed::More => Ref::More,
        Parsed::Bad(c) => Ref::Bad(c),
    }
}

fn gen_head(r: &mut Rng) -> Vec<u8> {
    let methods = ["GET", "POST", "HEAD", "PUT", "get", "G ET", ""];
    let paths = ["/", "/b/slug", "/edit/12/sync", "/search?q=a%20b", "/x/y/z?a=1&b=2", "bad", "/a b", "/\u{e9}"];
    let names = ["Host", "Cookie", "Content-Length", "X-Y", "Sec-Fetch-Site", "Bad Name", "", "A"];
    let values = ["localhost", "sid=abc; x=y", "12", "", "  spaced  ", "tab\tin", "caf\u{e9}", "a\u{1}b"];
    let mut s = String::new();
    s += if r.below(10) == 0 { r.pick(&methods) } else { r.pick(&methods[..3]) };
    s += " ";
    s += if r.below(10) == 0 { r.pick(&paths) } else { r.pick(&paths[..5]) };
    s += if r.below(20) == 0 { " HTTP/1.0\r\n" } else { " HTTP/1.1\r\n" };
    for _ in 0..r.below(8) {
        s += if r.below(10) == 0 { r.pick(&names) } else { r.pick(&names[..5]) };
        s += if r.below(15) == 0 { " " } else { ":" };
        s += if r.below(3) == 0 { " " } else { "" };
        s += if r.below(10) == 0 { r.pick(&values) } else { r.pick(&values[..5]) };
        s += if r.below(20) == 0 { "\n" } else { "\r\n" };
    }
    s += "\r\n";
    let mut b = s.into_bytes();
    // Corrupt some: a byte changed, dropped or cut off.
    match r.below(6) {
        0 if !b.is_empty() => {
            let i = r.below(b.len() as u64) as usize;
            b[i] = r.below(256) as u8;
        }
        1 if !b.is_empty() => {
            let i = r.below(b.len() as u64) as usize;
            b.remove(i);
        }
        2 => {
            let k = r.below(b.len() as u64 + 1) as usize;
            b.truncate(k);
        }
        _ => {}
    }
    if r.below(4) == 0 {
        b.extend_from_slice(b"body bytes after the head");
    }
    b
}

fn run() {
    let mut fails = 0;
    let mut hs: Vec<HeaderPos> = Vec::with_capacity(HEADERS_MAX);
    let mut r = Rng(0x9e3779b97f4a7c15);
    let mut done = 0;
    for t in 0..300_000 {
        let b = gen_head(&mut r);
        let want = reference(&b);
        let got = ours(&b, &mut hs);
        if got != want {
            fails += 1;
            if fails < 5 {
                println!("FAIL {t}: {:?}\n  ours {got:?}\n  ref  {want:?}", String::from_utf8_lossy(&b));
            }
        }
        if matches!(want, Ref::Done(..)) {
            done += 1;
        }
        // Byte by byte: "more" until whole, then the same answer.
        if t % 50 == 0 {
            for k in 0..b.len() {
                let g = ours(&b[..k], &mut hs);
                let w = reference(&b[..k]);
                if g != w {
                    fails += 1;
                    if fails < 5 {
                        println!("FAIL {t} prefix {k}: ours {g:?} ref {w:?}");
                    }
                }
            }
        }
    }
    println!("{} random heads ({done} whole and valid): {} differ from the reference", 300_000, fails);

    // 10x: a head of 80 KB (ten times the worst real one) is refused, at once.
    let mut big = b"GET / HTTP/1.1\r\n".to_vec();
    while big.len() < 80 * 1024 {
        big.extend_from_slice(b"Cookie: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n");
    }
    big.extend_from_slice(b"\r\n");
    let t = std::time::Instant::now();
    let got = ours(&big, &mut hs);
    let us = t.elapsed().as_secs_f64() * 1e6;
    let ok = got == Ref::Bad(431) && us < 1000.0;
    if !ok {
        fails += 1;
    }
    println!("{} an 80 KB head (10x the worst real one): {got:?} in {us:.1} us", if ok { "PASS" } else { "FAIL" });

    // Growth: heads of 1.6 KB and 16 KB (the limit); time must grow ~10x.
    let head_of = |bytes: usize| {
        let mut h = b"GET /a HTTP/1.1\r\n".to_vec();
        let mut i = 0;
        while h.len() + 40 < bytes && i < HEADERS_MAX {
            h.extend_from_slice(format!("X-Header-{i:03}: {}\r\n", "v".repeat((bytes / HEADERS_MAX).saturating_sub(20).max(1))).as_bytes());
            i += 1;
        }
        h.extend_from_slice(b"\r\n");
        h
    };
    let time_of = |h: &[u8], hs: &mut Vec<HeaderPos>| {
        let reps = 20_000;
        let t = std::time::Instant::now();
        for _ in 0..reps {
            std::hint::black_box(parse(std::hint::black_box(h), hs));
        }
        t.elapsed().as_secs_f64() / reps as f64
    };
    let small = head_of(1600);
    let large = head_of(16_000);
    let (ts, tl) = (time_of(&small, &mut hs), time_of(&large, &mut hs));
    let ratio = tl / ts;
    let gbps = large.len() as f64 / tl / 1e9;
    let ok = ratio < 20.0 && matches!(ours(&large, &mut hs), Ref::Done(..));
    if !ok {
        fails += 1;
    }
    println!(
        "{} growth: {} B in {:.2} us, {} B in {:.2} us ({ratio:.1}x for {:.1}x the bytes; {gbps:.2} GB/s)",
        if ok { "PASS" } else { "FAIL" },
        small.len(),
        ts * 1e6,
        large.len(),
        tl * 1e6,
        large.len() as f64 / small.len() as f64
    );
    println!("\n{fails} failure(s)");
    std::process::exit(if fails > 0 { 1 } else { 0 });
}
