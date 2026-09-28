// What a simulated browser sends and reads: HTTP requests (one per
// connection), responses, and a passkey authenticator (WebAuthn's "none"
// attestation, ES256, as a platform authenticator makes them).
use crate::sys::crypto::{p256_public, p256_sign, sha256};
use crate::webauthn::b64url_encode;

pub struct Req {
    pub method: &'static str,
    pub path: String,
    pub sid: Option<String>,
    pub body: Vec<u8>,
    pub ctype: &'static str,
    /// Who sends it (the oracle's X-Sim header: user index).
    pub user: usize,
    /// Asks to keep the connection (a browser's default); else Connection: close.
    pub keep: bool,
    /// Sent by Datastar (the page's script), as a browser's live parts do.
    pub ds: bool,
}

impl Req {
    pub fn get(user: usize, path: String, sid: &Option<String>) -> Req {
        Req { method: "GET", path, sid: sid.clone(), body: vec![], ctype: "", user, keep: false, ds: false }
    }

    /// Another method, without a body (HEAD, PUT...).
    pub fn method(method: &'static str, user: usize, path: String, sid: &Option<String>) -> Req {
        Req { method, path, sid: sid.clone(), body: vec![], ctype: "", user, keep: false, ds: false }
    }

    pub fn datastar(mut self, on: bool) -> Req {
        self.ds = on;
        self
    }

    pub fn form(user: usize, path: String, sid: &Option<String>, fields: &[(&str, &str)]) -> Req {
        Req { method: "POST", path, sid: sid.clone(), body: form(fields).into_bytes(), ctype: "application/x-www-form-urlencoded", user, keep: false, ds: false }
    }

    pub fn bytes(user: usize, path: String, sid: &Option<String>, body: Vec<u8>) -> Req {
        Req { method: "POST", path, sid: sid.clone(), body, ctype: "application/octet-stream", user, keep: false, ds: false }
    }

    pub fn wire(&self) -> Vec<u8> {
        let mut h = format!("{} {} HTTP/1.1\r\nHost: sim\r\n{}X-Sim: {}\r\n", self.method, self.path, if self.keep { "" } else { "Connection: close\r\n" }, self.user);
        if let Some(s) = &self.sid {
            h.push_str(&format!("Cookie: sid={s}\r\n"));
        }
        if self.ds {
            h.push_str("Datastar-Request: true\r\n");
        }
        if self.method == "POST" {
            h.push_str(&format!("Sec-Fetch-Site: same-origin\r\nContent-Type: {}\r\nContent-Length: {}\r\n", self.ctype, self.body.len()));
        }
        h.push_str("\r\n");
        let mut b = h.into_bytes();
        b.extend_from_slice(&self.body);
        b
    }
}

pub fn form(fields: &[(&str, &str)]) -> String {
    let mut s = String::new();
    for (i, (k, v)) in fields.iter().enumerate() {
        if i > 0 {
            s.push('&');
        }
        s.push_str(k);
        s.push('=');
        for b in v.bytes() {
            if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
                s.push(b as char);
            } else {
                s.push_str(&format!("%{b:02X}"));
            }
        }
    }
    s
}

/// Character references in an attribute: the named ones pages write, and
/// numeric ones.
fn decode(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        let r = &rest[i..];
        let (c, n) = if let Some(e) = r.find(';').filter(|&e| e < 12) {
            let name = &r[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "#39" | "apos" => Some('\''),
                _ if name.starts_with("#x") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            (c, e + 1)
        } else {
            (None, 1)
        };
        match c {
            Some(c) => out.push(c),
            None => out.push_str(&r[..n]),
        }
        rest = &r[n..];
    }
    out.push_str(rest);
    out
}

pub struct Resp {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
}

/// The length of a whole response at the start of b (its head and its
/// Content-Length of body), once it has all arrived.
pub fn whole(b: &[u8], head_only: bool) -> Option<usize> {
    let e = b.windows(4).position(|w| w == b"\r\n\r\n")?;
    let head = std::str::from_utf8(&b[..e]).ok()?;
    let n: usize = head.lines().find_map(|l| {
        let (k, v) = l.split_once(':')?;
        if k.eq_ignore_ascii_case("content-length") { v.trim().parse().ok() } else { None }
    })?;
    // (An answer to HEAD says its length and sends no body.)
    let n = if head_only { 0 } else { n };
    if b.len() >= e + 4 + n { Some(e + 4 + n) } else { None }
}

impl Resp {
    /// A whole response (the server closes after it: Connection: close).
    pub fn parse(b: &[u8]) -> Option<Resp> {
        let e = b.windows(4).position(|w| w == b"\r\n\r\n")?;
        let head = String::from_utf8_lossy(&b[..e]).to_string();
        let status = head.get(9..12)?.parse().ok()?;
        let body = b[e + 4..].to_vec();
        let r = Resp { status, head, body };
        match r.header("content-length").and_then(|v| v.parse::<usize>().ok()) {
            Some(n) if n != r.body.len() && !r.body.is_empty() => None,
            _ => Some(r),
        }
    }

    pub fn header(&self, name: &str) -> Option<String> {
        self.head.lines().find_map(|l| {
            let (k, v) = l.split_once(':')?;
            if k.eq_ignore_ascii_case(name) { Some(v.trim().to_string()) } else { None }
        })
    }

    pub fn sid(&self) -> Option<String> {
        self.head.lines().find_map(|l| {
            let v = l.strip_prefix("Set-Cookie: sid=")?;
            let t = v.split(';').next()?;
            if t.len() == 64 { Some(t.to_string()) } else { None }
        })
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// An attribute of the element with this id, as a browser reads it:
    /// the element's start tag parsed into its attributes, character
    /// references decoded. None if there is no such element or attribute.
    pub fn attr(&self, id: &str, name: &str) -> Option<String> {
        let t = self.text();
        let at = t.find(&format!(" id=\"{id}\""))?;
        let start = t[..at].rfind('<')?;
        let end = at + t[at..].find('>')?;
        let tag = &t[start + 1..end];
        // name="value" pairs after the element's name.
        let mut rest = tag.split_once(|c: char| c.is_whitespace())?.1;
        loop {
            rest = rest.trim_start();
            if rest.is_empty() || rest == "/" {
                return None;
            }
            let n = rest.find(|c: char| c == '=' || c.is_whitespace()).unwrap_or(rest.len());
            let (k, after) = rest.split_at(n);
            let (v, next) = match after.strip_prefix("=\"") {
                Some(a) => {
                    let e = a.find('"')?;
                    (Some(&a[..e]), &a[e + 1..])
                }
                None => (None, after),
            };
            if k == name {
                return Some(decode(v.unwrap_or("")));
            }
            rest = next;
        }
    }

    /// A string field of a JSON object answer, read as a browser's
    /// JSON.parse would: the whole body must be valid JSON.
    pub fn json(&self, key: &str) -> Option<String> {
        match parse_json(&self.body)? {
            Json::Obj(fields) => fields.into_iter().find(|(k, _)| k == key).and_then(|(_, v)| if let Json::Str(s) = v { Some(s) } else { None }),
            _ => None,
        }
    }
}

// STRICT JSON (RFC 8259, as JSON.parse reads it).
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

/// The whole text as one JSON value (whitespace around it), or None.
pub fn parse_json(b: &[u8]) -> Option<Json> {
    let mut i = 0;
    let v = value(b, &mut i, 0)?;
    ws(b, &mut i);
    if i == b.len() { Some(v) } else { None }
}

fn ws(b: &[u8], i: &mut usize) {
    while *i < b.len() && matches!(b[*i], b' ' | b'\t' | b'\n' | b'\r') {
        *i += 1;
    }
}

fn value(b: &[u8], i: &mut usize, depth: usize) -> Option<Json> {
    if depth > 64 {
        return None;
    }
    ws(b, i);
    match *b.get(*i)? {
        b'{' => {
            *i += 1;
            let mut f = vec![];
            ws(b, i);
            if b.get(*i) == Some(&b'}') {
                *i += 1;
                return Some(Json::Obj(f));
            }
            loop {
                ws(b, i);
                let Json::Str(k) = string(b, i)? else { return None };
                ws(b, i);
                if b.get(*i) != Some(&b':') {
                    return None;
                }
                *i += 1;
                f.push((k, value(b, i, depth + 1)?));
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b'}' => {
                        *i += 1;
                        return Some(Json::Obj(f));
                    }
                    _ => return None,
                }
            }
        }
        b'[' => {
            *i += 1;
            let mut a = vec![];
            ws(b, i);
            if b.get(*i) == Some(&b']') {
                *i += 1;
                return Some(Json::Arr(a));
            }
            loop {
                a.push(value(b, i, depth + 1)?);
                ws(b, i);
                match b.get(*i)? {
                    b',' => *i += 1,
                    b']' => {
                        *i += 1;
                        return Some(Json::Arr(a));
                    }
                    _ => return None,
                }
            }
        }
        b'"' => string(b, i),
        b't' if b[*i..].starts_with(b"true") => {
            *i += 4;
            Some(Json::Bool(true))
        }
        b'f' if b[*i..].starts_with(b"false") => {
            *i += 5;
            Some(Json::Bool(false))
        }
        b'n' if b[*i..].starts_with(b"null") => {
            *i += 4;
            Some(Json::Null)
        }
        b'-' | b'0'..=b'9' => {
            let s = *i;
            if b[*i] == b'-' {
                *i += 1;
            }
            let digits = |b: &[u8], i: &mut usize| {
                let s = *i;
                while *i < b.len() && b[*i].is_ascii_digit() {
                    *i += 1;
                }
                *i > s
            };
            if b.get(*i) == Some(&b'0') {
                *i += 1;
            } else if !digits(b, i) {
                return None;
            }
            if b.get(*i) == Some(&b'.') {
                *i += 1;
                if !digits(b, i) {
                    return None;
                }
            }
            if matches!(b.get(*i), Some(b'e' | b'E')) {
                *i += 1;
                if matches!(b.get(*i), Some(b'+' | b'-')) {
                    *i += 1;
                }
                if !digits(b, i) {
                    return None;
                }
            }
            std::str::from_utf8(&b[s..*i]).ok()?.parse().ok().map(Json::Num)
        }
        _ => None,
    }
}

fn string(b: &[u8], i: &mut usize) -> Option<Json> {
    if b.get(*i) != Some(&b'"') {
        return None;
    }
    *i += 1;
    let mut out: Vec<u16> = vec![];
    let mut raw: Vec<u8> = vec![];
    let flush = |raw: &mut Vec<u8>, out: &mut Vec<u16>| -> Option<()> {
        out.extend(std::str::from_utf8(raw).ok()?.encode_utf16());
        raw.clear();
        Some(())
    };
    loop {
        let c = *b.get(*i)?;
        *i += 1;
        match c {
            b'"' => {
                flush(&mut raw, &mut out)?;
                return String::from_utf16(&out).ok().map(Json::Str);
            }
            b'\\' => {
                flush(&mut raw, &mut out)?;
                let e = *b.get(*i)?;
                *i += 1;
                match e {
                    b'"' => out.push(b'"' as u16),
                    b'\\' => out.push(b'\\' as u16),
                    b'/' => out.push(b'/' as u16),
                    b'b' => out.push(8),
                    b'f' => out.push(12),
                    b'n' => out.push(10),
                    b'r' => out.push(13),
                    b't' => out.push(9),
                    b'u' => {
                        let h = std::str::from_utf8(b.get(*i..*i + 4)?).ok()?;
                        out.push(u16::from_str_radix(h, 16).ok()?);
                        *i += 4;
                    }
                    _ => return None,
                }
            }
            0..=0x1f => return None,
            _ => raw.push(c),
        }
    }
}

/// A passkey on a simulated device.
pub struct Passkey {
    pub key: [u8; 32],
    pub public: [u8; 64],
    pub id: Vec<u8>,
    pub count: u32,
}

impl Passkey {
    pub fn new(random: &mut dyn FnMut() -> u64) -> Passkey {
        loop {
            let mut key = [0u8; 32];
            for c in key.chunks_mut(8) {
                c.copy_from_slice(&random().to_le_bytes());
            }
            if let Some(public) = p256_public(&key) {
                let id = (0..16).map(|_| random() as u8).collect();
                return Passkey { key, public, id, count: 0 };
            }
        }
    }

    fn client_data(kind: &str, challenge: &str) -> Vec<u8> {
        format!("{{\"type\":\"{kind}\",\"challenge\":\"{challenge}\",\"origin\":\"{}\",\"crossOrigin\":false}}", super::ORIGIN).into_bytes()
    }

    fn auth_data(&self, flags: u8, attested: &[u8]) -> Vec<u8> {
        let mut a = sha256(super::RP_ID.as_bytes()).to_vec();
        a.push(flags);
        a.extend_from_slice(&self.count.to_be_bytes());
        a.extend_from_slice(attested);
        a
    }

    /// Registration: (cd, att), base64url, for the options' challenge.
    pub fn register(&self, challenge: &str) -> (String, String) {
        let cd = Passkey::client_data("webauthn.create", challenge);
        let mut att_cred = vec![0u8; 16];
        att_cred.extend_from_slice(&(self.id.len() as u16).to_be_bytes());
        att_cred.extend_from_slice(&self.id);
        att_cred.extend_from_slice(b"\xa5\x01\x02\x03\x26\x20\x01\x21\x58\x20");
        att_cred.extend_from_slice(&self.public[..32]);
        att_cred.extend_from_slice(b"\x22\x58\x20");
        att_cred.extend_from_slice(&self.public[32..]);
        let ad = self.auth_data(0x45, &att_cred);
        let mut att = b"\xa3\x63fmt\x64none\x67attStmt\xa0\x68authData".to_vec();
        if ad.len() < 256 {
            att.extend_from_slice(&[0x58, ad.len() as u8]);
        } else {
            att.push(0x59);
            att.extend_from_slice(&(ad.len() as u16).to_be_bytes());
        }
        att.extend_from_slice(&ad);
        (b64url_encode(&cd), b64url_encode(&att))
    }

    /// Login: (id, cd, ad, sig), base64url; the counter moves on.
    pub fn assert(&mut self, challenge: &str, nonce: [u8; 32]) -> Option<(String, String, String, String)> {
        self.count += 1;
        let cd = Passkey::client_data("webauthn.get", challenge);
        let ad = self.auth_data(0x05, &[]);
        let mut msg = ad.clone();
        msg.extend_from_slice(&sha256(&cd));
        let rs = p256_sign(&self.key, &nonce, &msg)?;
        Some((b64url_encode(&self.id), b64url_encode(&cd), b64url_encode(&ad), b64url_encode(&der(&rs))))
    }
}

/// r || s as DER: SEQUENCE { INTEGER r, INTEGER s }.
fn der(rs: &[u8; 64]) -> Vec<u8> {
    let int = |x: &[u8]| {
        let mut v: Vec<u8> = x.iter().copied().skip_while(|&b| b == 0).collect();
        if v.is_empty() || v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut o = vec![0x02, v.len() as u8];
        o.extend_from_slice(&v);
        o
    };
    let mut body = int(&rs[..32]);
    body.extend_from_slice(&int(&rs[32..]));
    let mut o = vec![0x30, body.len() as u8];
    o.extend_from_slice(&body);
    o
}
