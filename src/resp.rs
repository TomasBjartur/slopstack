// Writing responses: status, the security headers on every response (the
// CSP allows only same-origin scripts, styles and connections, no inline
// script or style; HTML pages add a fresh nonce, which Datastar uses for
// the page's data-* expressions; no page allows 'unsafe-eval'), caching,
// cookies, redirects, and Datastar fragment patches (server-sent events).
use crate::app::itoa;

pub const SECURITY: &str = "X-Content-Type-Options: nosniff\r\nReferrer-Policy: same-origin\r\nCross-Origin-Opener-Policy: same-origin\r\nCross-Origin-Resource-Policy: same-origin\r\n";
pub const CSP: &str = "Content-Security-Policy: default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' https:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n";

/// How a page may be cached: no-store (it shows who is signed in),
/// revalidate (a public page: the browser may keep it for Back), or
/// immutable (a versioned asset).
#[derive(Clone, Copy, PartialEq)]
pub enum Cache {
    NoStore,
    Revalidate,
    Immutable,
}

fn reason(code: u16) -> &'static str {
    match code {
        200 => "OK",
        204 => "No Content",
        303 => "See Other",
        304 => "Not Modified",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        503 => "Service Unavailable",
        _ => "Error",
    }
}

/// A response head up to (not including) its length and the blank line.
pub fn head(out: &mut Vec<u8>, code: u16, ctype: &str, cache: Cache, nonce: Option<&str>) {
    head_wasm(out, code, ctype, cache, nonce, false)
}

/// As head; wasm: the page may compile WebAssembly (the editor's:
/// 'wasm-unsafe-eval' allows that and nothing else, not eval).
pub fn head_wasm(out: &mut Vec<u8>, code: u16, ctype: &str, cache: Cache, nonce: Option<&str>, wasm: bool) {
    let mut n = [0u8; 20];
    out.extend_from_slice(b"HTTP/1.1 ");
    out.extend_from_slice(itoa(code as u64, &mut n));
    out.push(b' ');
    out.extend_from_slice(reason(code).as_bytes());
    out.extend_from_slice(b"\r\nContent-Type: ");
    out.extend_from_slice(ctype.as_bytes());
    out.extend_from_slice(b"\r\n");
    match nonce {
        Some(nc) => {
            out.extend_from_slice(b"Content-Security-Policy: default-src 'none'; script-src 'self' 'nonce-");
            out.extend_from_slice(nc.as_bytes());
            if wasm {
                out.extend_from_slice(b"' 'wasm-unsafe-eval");
            }
            out.extend_from_slice(b"'; style-src 'self'; img-src 'self' https:; connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'\r\n");
        }
        None => out.extend_from_slice(CSP.as_bytes()),
    }
    out.extend_from_slice(SECURITY.as_bytes());
    out.extend_from_slice(match cache {
        Cache::NoStore => b"Cache-Control: no-store\r\n".as_slice(),
        Cache::Revalidate => b"Cache-Control: private, no-cache\r\n",
        Cache::Immutable => b"Cache-Control: public, max-age=31536000, immutable\r\n",
    });
}

/// Ends the head with the length, then the body.
pub fn body(out: &mut Vec<u8>, body: &[u8], send_body: bool) {
    let mut n = [0u8; 20];
    out.extend_from_slice(b"Content-Length: ");
    out.extend_from_slice(itoa(body.len() as u64, &mut n));
    out.extend_from_slice(b"\r\n\r\n");
    if send_body {
        out.extend_from_slice(body);
    }
}

/// A whole response with a body.
pub fn whole(out: &mut Vec<u8>, code: u16, ctype: &str, cache: Cache, nonce: Option<&str>, extra: &[u8], b: &[u8], send_body: bool) {
    head(out, code, ctype, cache, nonce);
    out.extend_from_slice(extra);
    body(out, b, send_body);
}

/// 303 to location (a path on this site), with extra headers (cookies).
pub fn redirect(out: &mut Vec<u8>, location: &str, extra: &[u8]) {
    debug_assert!(location.starts_with('/') && !location.starts_with("//"));
    head(out, 303, "text/plain; charset=utf-8", Cache::NoStore, None);
    out.extend_from_slice(b"Location: ");
    out.extend_from_slice(location.as_bytes());
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(extra);
    body(out, b"", true);
}

/// A session cookie (64 hex: the token), or the header that clears it.
pub fn cookie(token_hex: Option<&str>, secure: bool) -> Vec<u8> {
    let mut c = b"Set-Cookie: sid=".to_vec();
    match token_hex {
        Some(t) => {
            c.extend_from_slice(t.as_bytes());
            c.extend_from_slice(b"; Path=/; HttpOnly; SameSite=Strict; Max-Age=2592000");
        }
        None => c.extend_from_slice(b"; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
    }
    if secure {
        c.extend_from_slice(b"; Secure");
    }
    c.extend_from_slice(b"\r\n");
    c
}

/// Datastar events: each (selector, mode, html) patches the page. The HTML
/// is ours (templates), one data: elements line per line.
pub fn patches(out: &mut Vec<u8>, ps: &[(&str, &str, &[u8])]) {
    let mut b: Vec<u8> = Vec::new();
    for (sel, mode, html) in ps {
        b.extend_from_slice(b"event: datastar-patch-elements\n");
        if !sel.is_empty() {
            b.extend_from_slice(b"data: selector ");
            b.extend_from_slice(sel.as_bytes());
            b.push(b'\n');
        }
        if !mode.is_empty() {
            b.extend_from_slice(b"data: mode ");
            b.extend_from_slice(mode.as_bytes());
            b.push(b'\n');
        }
        for line in html.split(|&c| c == b'\n') {
            b.extend_from_slice(b"data: elements ");
            b.extend_from_slice(line);
            b.push(b'\n');
        }
        b.push(b'\n');
    }
    whole(out, 200, "text/event-stream", Cache::NoStore, None, b"", &b, true);
}

pub fn hex(b: &[u8]) -> String {
    const H: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(b.len() * 2);
    for &x in b {
        s.push(H[(x >> 4) as usize] as char);
        s.push(H[(x & 15) as usize] as char);
    }
    s
}

pub fn unhex(s: &[u8]) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    let v = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    };
    s.chunks(2).map(|p| Some(v(p[0])? << 4 | v(p[1])?)).collect()
}
