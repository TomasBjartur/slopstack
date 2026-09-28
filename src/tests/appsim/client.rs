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
}

impl Req {
    pub fn get(user: usize, path: String, sid: &Option<String>) -> Req {
        Req { method: "GET", path, sid: sid.clone(), body: vec![], ctype: "", user }
    }

    pub fn form(user: usize, path: String, sid: &Option<String>, fields: &[(&str, &str)]) -> Req {
        Req { method: "POST", path, sid: sid.clone(), body: form(fields).into_bytes(), ctype: "application/x-www-form-urlencoded", user }
    }

    pub fn bytes(user: usize, path: String, sid: &Option<String>, body: Vec<u8>) -> Req {
        Req { method: "POST", path, sid: sid.clone(), body, ctype: "application/octet-stream", user }
    }

    pub fn wire(&self) -> Vec<u8> {
        let mut h = format!("{} {} HTTP/1.1\r\nHost: sim\r\nConnection: close\r\nX-Sim: {}\r\n", self.method, self.path, self.user);
        if let Some(s) = &self.sid {
            h.push_str(&format!("Cookie: sid={s}\r\n"));
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

pub struct Resp {
    pub status: u16,
    pub head: String,
    pub body: Vec<u8>,
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
            Some(n) if n != r.body.len() => None,
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

    /// The value of attribute name="..." (first), in an HTML body.
    pub fn attr(&self, name: &str) -> Option<String> {
        let t = self.text();
        let k = format!("{name}=\"");
        let i = t.find(&k)? + k.len();
        let j = t[i..].find('"')? + i;
        Some(t[i..j].to_string())
    }

    /// A JSON string field (the passkey options: flat, no escapes needed).
    pub fn json(&self, key: &str) -> Option<String> {
        let t = self.text();
        let k = format!("\"{key}\":\"");
        let i = t.find(&k)? + k.len();
        let j = t[i..].find('"')? + i;
        Some(t[i..j].to_string())
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
