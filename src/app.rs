// The application the event loop is tested with (tests/sim.rs). GET
// /echo/<x> answers <x> (tests check order with it); GET /park/<ms>/<x>
// answers <x> after ms (parked meanwhile; "never": not answered); GET / a
// small page; HEAD as GET without the body.
use crate::server::{error, App, Ctx, Request};

#[derive(Default)]
pub struct Site {
    /// Parked requests: (connection, when to answer, the answer's body).
    parked: Vec<(u64, u64, Vec<u8>)>,
}

fn ok(out: &mut Vec<u8>, body: &[u8], head: bool) {
    out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ");
    let mut n = [0u8; 20];
    out.extend_from_slice(itoa(body.len() as u64, &mut n));
    out.extend_from_slice(b"\r\n\r\n");
    if !head {
        out.extend_from_slice(body);
    }
}

impl App for Site {
    fn gone(&mut self, conn: u64) {
        self.parked.retain(|(c, _, _)| *c != conn);
    }

    fn ready(&mut self, now_ms: u64, answers: &mut Vec<(u64, Vec<u8>)>) {
        self.parked.retain(|(conn, due, body)| {
            if now_ms < *due {
                return true;
            }
            let mut out = vec![];
            ok(&mut out, body, false);
            answers.push((*conn, out));
            false
        });
    }

    fn handle(&mut self, req: &Request, cx: &mut Ctx, out: &mut Vec<u8>) -> bool {
        let head = req.method == b"HEAD";
        if let Some(rest) = req.target.strip_prefix(b"/park/") {
            let text = std::str::from_utf8(rest).unwrap_or("");
            let (ms, x) = text.split_once('/').unwrap_or(("0", ""));
            let due = if ms == "never" { u64::MAX } else { cx.mono_ms + ms.parse::<u64>().unwrap_or(0) };
            self.parked.push((cx.conn, due, x.as_bytes().to_vec()));
            cx.park = true;
            return true;
        }
        if req.method == b"POST" {
            // (Tests: the body back.)
            if req.target == b"/echo" {
                out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: application/octet-stream\r\nContent-Length: ");
                let mut n = [0u8; 20];
                out.extend_from_slice(itoa(req.body.len() as u64, &mut n));
                out.extend_from_slice(b"\r\n\r\n");
                out.extend_from_slice(req.body);
                return true;
            }
            error(out, 404);
            return false;
        }
        let body: &[u8] = if req.target == b"/" {
            b"<!doctype html><title>Hello</title><p>Hello.\n"
        } else if let Some(x) = req.target.strip_prefix(b"/echo/") {
            x
        } else {
            error(out, 404);
            return true;
        };
        ok(out, body, head);
        true
    }
}

/// A number in decimal, without allocating.
pub fn itoa(mut v: u64, buf: &mut [u8; 20]) -> &[u8] {
    let mut i = 20;
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            return &buf[i..];
        }
    }
}
