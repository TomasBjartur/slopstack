// LAWS: when a passkey registration or login is accepted (owned by people;
// a change here is a SECURITY DECISION). W3C WebAuthn Level 3, 7.1 and 7.2.
//
// Not here, and enforced by the database layer (src/db.rs, tested): the
// challenge was issued by this server, is unexpired and unused, has the
// right purpose (and, for registration, is bound to the emailed link in
// use), and is consumed in the same transaction that makes the session.
use vstd::prelude::*;

verus! {

/// What the browser reported (clientDataJSON). cross: crossOrigin was true,
/// or a topOrigin was present (an iframe).
pub struct Client {
    pub kind: Seq<u8>,
    pub origin: Seq<u8>,
    pub cross: bool,
}

/// What the authenticator reported (authenticator data).
pub struct Auth {
    pub rp_hash: Seq<u8>,
    pub flags: u8,
    pub count: u32,
}

/// What this server expects: its origin, and the SHA-256 of its RP ID.
pub struct Expect {
    pub origin: Seq<u8>,
    pub rp_hash: Seq<u8>,
}

/// "webauthn.create" and "webauthn.get"
pub open spec fn create() -> Seq<u8> {
    seq![119u8, 101, 98, 97, 117, 116, 104, 110, 46, 99, 114, 101, 97, 116, 101]
}
pub open spec fn get() -> Seq<u8> {
    seq![119u8, 101, 98, 97, 117, 116, 104, 110, 46, 103, 101, 116]
}

/// User present, user verified (PIN or biometric), attested credential data.
pub open spec fn up(f: u8) -> bool { f & 1 != 0 }
pub open spec fn uv(f: u8) -> bool { f & 4 != 0 }
pub open spec fn at(f: u8) -> bool { f & 64 != 0 }

pub open spec fn client_ok(e: Expect, c: Client, kind: Seq<u8>) -> bool {
    c.kind == kind && c.origin == e.origin && !c.cross
}

pub open spec fn auth_ok(e: Expect, a: Auth) -> bool {
    a.rp_hash == e.rp_hash && up(a.flags) && uv(a.flags)
}

/// The signature counter advances, or the authenticator keeps none (both 0).
pub open spec fn counter_ok(stored: u32, new: u32) -> bool {
    (stored == 0 && new == 0) || new > stored
}

pub open spec fn register_ok(e: Expect, c: Client, a: Auth) -> bool {
    client_ok(e, c, create()) && auth_ok(e, a) && at(a.flags)
}

/// sig: the signature verified with the credential's stored public key.
pub open spec fn login_ok(e: Expect, c: Client, a: Auth, sig: bool, stored: u32) -> bool {
    client_ok(e, c, get()) && auth_ok(e, a) && sig && counter_ok(stored, a.count)
}

} // verus!
