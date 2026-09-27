// The application (for now: the spike's pages). GET /echo/<x> answers <x>
// (tests check order with it); GET / a small page; HEAD as GET without
// the body.
use crate::server::{error, App, Request};

pub struct Site;

impl App for Site {
    fn handle(&mut self, req: &Request, _now_ms: u64, out: &mut Vec<u8>) -> bool {
        let head = req.method == b"HEAD";
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
        out.extend_from_slice(b"HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: ");
        let mut n = [0u8; 20];
        out.extend_from_slice(itoa(body.len() as u64, &mut n));
        out.extend_from_slice(b"\r\n\r\n");
        if !head {
            out.extend_from_slice(body);
        }
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
