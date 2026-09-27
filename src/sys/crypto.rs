// Cryptography, from HACL* (vendor/hacl: formally verified C: memory safe,
// functionally correct, constant time). Declared by hand; each wrapper
// passes buffers of exactly the sizes HACL* documents.
use std::os::raw::c_uint;

extern "C" {
    fn Hacl_Hash_SHA2_hash_256(output: *mut u8, input: *mut u8, input_len: c_uint);
    fn Hacl_P256_ecdsa_verif_p256_sha2(msg_len: c_uint, msg: *mut u8, public_key: *mut u8, r: *mut u8, s: *mut u8) -> bool;
    fn Hacl_P256_uncompressed_to_raw(pk: *mut u8, pk_raw: *mut u8) -> bool;
    fn Hacl_P256_validate_public_key(pk: *mut u8) -> bool;
}

/// SHA-256 of data (at most 4 GiB).
pub fn sha256(data: &[u8]) -> [u8; 32] {
    assert!(data.len() <= u32::MAX as usize);
    let mut out = [0u8; 32];
    // SAFETY: out is 32 bytes; input is valid for input_len bytes. HACL* only
    // reads input (the pointer is not const in its signature).
    unsafe { Hacl_Hash_SHA2_hash_256(out.as_mut_ptr(), data.as_ptr() as *mut u8, data.len() as c_uint) };
    out
}

/// Verifies an ECDSA P-256 signature over SHA-256(msg). public_key: the
/// 64-byte x || y; sig: DER (as WebAuthn gives it). False on any doubt:
/// a malformed signature, an invalid key or point, a wrong signature.
pub fn p256_verify(public_key: &[u8; 64], msg: &[u8], der_sig: &[u8]) -> bool {
    let Some((r, s)) = der_rs(der_sig) else { return false };
    if msg.len() > u32::MAX as usize {
        return false;
    }
    let mut pk = *public_key;
    let (mut r, mut s) = (r, s);
    // SAFETY: pk is 64 bytes, r and s 32 each, msg valid for its length;
    // HACL* checks the key is a valid point and 0 < r, s < n.
    unsafe { Hacl_P256_ecdsa_verif_p256_sha2(msg.len() as c_uint, msg.as_ptr() as *mut u8, pk.as_mut_ptr(), r.as_mut_ptr(), s.as_mut_ptr()) }
}

/// A 65-byte uncompressed point (04 || x || y) to x || y; None unless it
/// is a valid public key (SP 800-56A: not the point at infinity,
/// coordinates in range, on the curve). (HACL*'s conversion alone does not
/// check the point: a test caught that.)
pub fn p256_raw(uncompressed: &[u8; 65]) -> Option<[u8; 64]> {
    let mut pk = *uncompressed;
    let mut raw = [0u8; 64];
    // SAFETY: 65 and 64 bytes, as documented.
    if !unsafe { Hacl_P256_uncompressed_to_raw(pk.as_mut_ptr(), raw.as_mut_ptr()) } {
        return None;
    }
    // SAFETY: 64 bytes, as documented.
    if unsafe { Hacl_P256_validate_public_key(raw.as_mut_ptr()) } { Some(raw) } else { None }
}

/// r and s from a DER ECDSA signature: SEQUENCE { INTEGER r, INTEGER s },
/// each a positive integer of at most 32 bytes (a leading zero allowed only
/// before a high bit), strict lengths, nothing after.
pub fn der_rs(d: &[u8]) -> Option<([u8; 32], [u8; 32])> {
    if d.len() < 8 || d.len() > 72 || d[0] != 0x30 || d[1] as usize != d.len() - 2 {
        return None;
    }
    let (r, rest) = der_int(&d[2..])?;
    let (s, rest) = der_int(rest)?;
    if !rest.is_empty() {
        return None;
    }
    Some((r, s))
}

fn der_int(d: &[u8]) -> Option<([u8; 32], &[u8])> {
    if d.len() < 3 || d[0] != 0x02 {
        return None;
    }
    let n = d[1] as usize;
    if n == 0 || n > 33 || d.len() < 2 + n {
        return None;
    }
    let mut v = &d[2..2 + n];
    if v[0] & 0x80 != 0 {
        return None; // negative
    }
    if v[0] == 0 {
        if n == 1 || v[1] & 0x80 == 0 {
            return None; // not minimal
        }
        v = &v[1..];
    }
    if v.len() > 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out[32 - v.len()..].copy_from_slice(v);
    Some((out, &d[2 + n..]))
}
