// Passkeys (WebAuthn). The decisions are proved (Verus, about this code)
// to be spec/webauthn.rs's register_ok and login_ok. The parsing that
// feeds them (CBOR, authenticator data, COSE keys, clientDataJSON,
// base64url) is strict, bounded, and tested (src/tests/webauthn.rs,
// tests/passkey_test.py: a software authenticator and 60+ attacks).
use vstd::prelude::*;
use crate::spec_webauthn::*;

verus! {

pub struct ClientData {
    pub kind: Vec<u8>,
    pub origin: Vec<u8>,
    pub challenge: Vec<u8>,
    pub cross: bool,
}

pub struct AuthData {
    pub rp_hash: Vec<u8>,
    pub flags: u8,
    pub count: u32,
}

pub struct Expected {
    pub origin: Vec<u8>,
    pub rp_hash: Vec<u8>,
}

pub open spec fn client_view(c: ClientData) -> Client {
    Client { kind: c.kind@, origin: c.origin@, cross: c.cross }
}
pub open spec fn auth_view(a: AuthData) -> Auth {
    Auth { rp_hash: a.rp_hash@, flags: a.flags, count: a.count }
}
pub open spec fn expect_view(e: Expected) -> Expect {
    Expect { origin: e.origin@, rp_hash: e.rp_hash@ }
}

fn eq(a: &[u8], b: &[u8]) -> (r: bool)
    ensures r == (a@ == b@),
{
    if a.len() != b.len() {
        return false;
    }
    let mut i: usize = 0;
    while i < a.len()
        invariant a.len() == b.len(), i <= a.len(), forall|j: int| 0 <= j < i ==> a@[j] == b@[j],
        decreases a.len() - i,
    {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    proof { assert(a@ =~= b@); }
    true
}

fn client_ok_exec(e: &Expected, c: &ClientData, kind: &[u8]) -> (r: bool)
    ensures r == client_ok(expect_view(*e), client_view(*c), kind@),
{
    eq(c.kind.as_slice(), kind) && eq(c.origin.as_slice(), e.origin.as_slice()) && !c.cross
}

fn auth_ok_exec(e: &Expected, a: &AuthData) -> (r: bool)
    ensures r == auth_ok(expect_view(*e), auth_view(*a)),
{
    eq(a.rp_hash.as_slice(), e.rp_hash.as_slice()) && a.flags & 1 != 0 && a.flags & 4 != 0
}

/// Accept this registration?
pub fn register_decision(e: &Expected, c: &ClientData, a: &AuthData) -> (r: bool)
    ensures r == register_ok(expect_view(*e), client_view(*c), auth_view(*a)),
{
    let kind: [u8; 15] = [119, 101, 98, 97, 117, 116, 104, 110, 46, 99, 114, 101, 97, 116, 101];
    assert(kind@ =~= create());
    client_ok_exec(e, c, &kind) && auth_ok_exec(e, a) && a.flags & 64 != 0
}

/// Accept this login? sig: the signature verified with the stored key.
pub fn login_decision(e: &Expected, c: &ClientData, a: &AuthData, sig: bool, stored: u32) -> (r: bool)
    ensures r == login_ok(expect_view(*e), client_view(*c), auth_view(*a), sig, stored),
{
    let kind: [u8; 12] = [119, 101, 98, 97, 117, 116, 104, 110, 46, 103, 101, 116];
    assert(kind@ =~= get());
    client_ok_exec(e, c, &kind) && auth_ok_exec(e, a) && sig && ((stored == 0 && a.count == 0) || a.count > stored)
}

} // verus!

/// A passkey check that passed: the only thing that lets the database
/// layer issue a session (src/db.rs Store::session_for). Private field:
/// made only by checked() below, after a proved decision said yes. (The
/// sessions law: no session without a passkey check, or an email link's
/// passkey registration, which is also a passkey check.)
pub struct Passed {
    _private: (),
}

/// Passed, if the decision (register_decision or login_decision) said yes.
pub fn checked(decision: bool) -> Option<Passed> {
    if decision { Some(Passed { _private: () }) } else { None }
}

// PARSING (tested, not proved)

/// base64url without padding (as WebAuthn and our pages write it); None
/// on any other byte or a bad length.
pub fn b64url(s: &[u8]) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return None,
        } as u32)
    };
    if s.len() % 4 == 1 {
        return None;
    }
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut v = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            v |= val(c)? << (18 - 6 * i);
        }
        out.push((v >> 16) as u8);
        if chunk.len() > 2 {
            out.push((v >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(v as u8);
        }
        // Unused low bits must be zero (one encoding per value).
        let used = match chunk.len() {
            2 => v & 0xffff,
            3 => v & 0xff,
            _ => 0,
        };
        if used != 0 {
            return None;
        }
    }
    Some(out)
}

pub fn b64url_encode(b: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::with_capacity(b.len() * 4 / 3 + 3);
    for c in b.chunks(3) {
        let v = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(A[(v >> 18) as usize & 63] as char);
        s.push(A[(v >> 12) as usize & 63] as char);
        if c.len() > 1 {
            s.push(A[(v >> 6) as usize & 63] as char);
        }
        if c.len() > 2 {
            s.push(A[v as usize & 63] as char);
        }
    }
    s
}

/// CBOR (RFC 8949), the part WebAuthn uses: definite lengths only, at
/// most 4 deep, at most 16 KB.
#[derive(Debug, PartialEq, Clone)]
pub enum Cbor {
    Int(i128),
    Bytes(Vec<u8>),
    Text(String),
    Arr(Vec<Cbor>),
    Map(Vec<(Cbor, Cbor)>),
    Bool(bool),
    Null,
}

pub fn cbor(b: &[u8]) -> Option<(Cbor, usize)> {
    if b.len() > 16 * 1024 {
        return None;
    }
    let mut i = 0;
    let v = cbor_item(b, &mut i, 0)?;
    Some((v, i))
}

fn cbor_item(b: &[u8], i: &mut usize, depth: u32) -> Option<Cbor> {
    if depth > 4 {
        return None;
    }
    let ib = *b.get(*i)?;
    *i += 1;
    let (major, info) = (ib >> 5, ib & 31);
    let arg: u64 = match info {
        0..=23 => info as u64,
        24 => {
            let v = *b.get(*i)? as u64;
            *i += 1;
            v
        }
        25 | 26 | 27 => {
            let n = 1usize << (info - 24);
            let s = b.get(*i..*i + n)?;
            *i += n;
            s.iter().fold(0u64, |a, &x| a << 8 | x as u64)
        }
        _ => return None, // indefinite lengths and reserved
    };
    match major {
        0 => Some(Cbor::Int(arg as i128)),
        1 => Some(Cbor::Int(-1 - arg as i128)),
        2 | 3 => {
            let n = usize::try_from(arg).ok()?;
            let s = b.get(*i..i.checked_add(n)?)?;
            *i += n;
            if major == 2 { Some(Cbor::Bytes(s.to_vec())) } else { Some(Cbor::Text(String::from_utf8(s.to_vec()).ok()?)) }
        }
        4 => {
            if arg > 64 {
                return None;
            }
            let mut v = vec![];
            for _ in 0..arg {
                v.push(cbor_item(b, i, depth + 1)?);
            }
            Some(Cbor::Arr(v))
        }
        5 => {
            if arg > 64 {
                return None;
            }
            let mut v = vec![];
            for _ in 0..arg {
                let k = cbor_item(b, i, depth + 1)?;
                let val = cbor_item(b, i, depth + 1)?;
                if v.iter().any(|(k2, _)| *k2 == k) {
                    return None; // duplicate keys
                }
                v.push((k, val));
            }
            Some(Cbor::Map(v))
        }
        7 => match info {
            20 => Some(Cbor::Bool(false)),
            21 => Some(Cbor::Bool(true)),
            22 => Some(Cbor::Null),
            _ => None,
        },
        _ => None, // tags and the rest: not used by WebAuthn
    }
}

fn map_get<'a>(m: &'a Cbor, key: &Cbor) -> Option<&'a Cbor> {
    if let Cbor::Map(kv) = m { kv.iter().find(|(k, _)| k == key).map(|(_, v)| v) } else { None }
}

/// Authenticator data: rpIdHash, flags, signCount, and (if AT) the
/// credential id and its public key (x || y, a valid P-256 point).
pub struct Parsed {
    pub auth: AuthData,
    pub cred_id: Vec<u8>,
    pub public_key: Option<[u8; 64]>,
}

pub fn auth_data(b: &[u8]) -> Option<Parsed> {
    if b.len() < 37 {
        return None;
    }
    let auth = AuthData { rp_hash: b[..32].to_vec(), flags: b[32], count: u32::from_be_bytes([b[33], b[34], b[35], b[36]]) };
    let mut cred_id = vec![];
    let mut public_key = None;
    let mut rest = &b[37..];
    if auth.flags & 64 != 0 {
        if rest.len() < 18 {
            return None;
        }
        let n = u16::from_be_bytes([rest[16], rest[17]]) as usize;
        if n < 16 || n > 1023 || rest.len() < 18 + n {
            return None;
        }
        cred_id = rest[18..18 + n].to_vec();
        let (key, used) = cbor(&rest[18 + n..])?;
        // COSE EC2 key: kty 2, alg -7 (ES256), crv 1 (P-256), x, y.
        let get = |k: i128| map_get(&key, &Cbor::Int(k));
        if get(1) != Some(&Cbor::Int(2)) || get(3) != Some(&Cbor::Int(-7)) || get(-1) != Some(&Cbor::Int(1)) {
            return None;
        }
        let (Some(Cbor::Bytes(x)), Some(Cbor::Bytes(y))) = (get(-2), get(-3)) else { return None };
        if x.len() != 32 || y.len() != 32 {
            return None;
        }
        let mut unc = [4u8; 65];
        unc[1..33].copy_from_slice(x);
        unc[33..].copy_from_slice(y);
        public_key = Some(crate::sys::crypto::p256_raw(&unc)?);
        rest = &rest[18 + n + used..];
    }
    // Extensions (ED): not asked for; data after this is refused.
    if auth.flags & 128 != 0 || !rest.is_empty() {
        return None;
    }
    Some(Parsed { auth, cred_id, public_key })
}

/// The attestation object's authenticator data. We ask for no
/// attestation (attestation: "none"), so fmt must be "none" with an empty
/// attStmt: a statement we would not verify is refused, not ignored.
pub fn attestation_auth_data(att: &[u8]) -> Option<Vec<u8>> {
    let (m, used) = cbor(att)?;
    if used != att.len() {
        return None;
    }
    if map_get(&m, &Cbor::Text("fmt".into())) != Some(&Cbor::Text("none".into())) {
        return None;
    }
    match map_get(&m, &Cbor::Text("attStmt".into())) {
        Some(Cbor::Map(kv)) if kv.is_empty() => {}
        _ => return None,
    }
    match map_get(&m, &Cbor::Text("authData".into())) {
        Some(Cbor::Bytes(b)) => Some(b.clone()),
        _ => None,
    }
}

/// clientDataJSON: type, challenge (decoded), origin, and whether it came
/// from another origin's frame (crossOrigin true, or a topOrigin).
pub fn client_data(b: &[u8]) -> Option<ClientData> {
    if b.len() > 4096 {
        return None;
    }
    let j = crate::json::parse(b)?;
    let kind = j.get("type")?.str()?.as_bytes().to_vec();
    let challenge = b64url(j.get("challenge")?.str()?.as_bytes())?;
    let origin = j.get("origin")?.str()?.as_bytes().to_vec();
    let cross = matches!(j.get("crossOrigin"), Some(crate::json::Json::Bool(true))) || j.get("topOrigin").is_some();
    Some(ClientData { kind, origin, challenge, cross })
}
