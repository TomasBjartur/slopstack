// The crypto binding (src/sys/crypto.rs): published vectors, tampering,
// and malformed signatures (refused, never a crash).
use crate::sys::crypto::*;

fn hex(s: &str) -> Vec<u8> {
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect()
}

/// DER of (r, s), minimal.
fn der(r: &[u8], s: &[u8]) -> Vec<u8> {
    let int = |v: &[u8]| {
        let mut v: Vec<u8> = v.iter().copied().skip_while(|&b| b == 0).collect();
        if v.is_empty() || v[0] & 0x80 != 0 {
            v.insert(0, 0);
        }
        let mut o = vec![0x02, v.len() as u8];
        o.extend(v);
        o
    };
    let body = [int(r), int(s)].concat();
    let mut o = vec![0x30, body.len() as u8];
    o.extend(body);
    o
}

pub fn run() {
    let mut fails = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            fails += 1;
        }
    };
    check("sha256(\"abc\")", sha256(b"abc").to_vec() == hex("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
    check("sha256(\"\")", sha256(b"").to_vec() == hex("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"));
    let million = vec![b'a'; 1_000_000];
    check("sha256(a million a's)", sha256(&million).to_vec() == hex("cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"));

    // RFC 6979, A.2.5: P-256, SHA-256, message "sample".
    let mut pk = [0u8; 64];
    pk[..32].copy_from_slice(&hex("60FED4BA255A9D31C961EB74C6356D68C049B8923B61FA6CE669622E60F29FB6"));
    pk[32..].copy_from_slice(&hex("7903FE1008B8BC99A41AE9E95628BC64F2F1B20C2D7E9F5177A3C294D4462299"));
    let r = hex("EFD48B2AACB6A8FD1140DD9CD45E81D69D2C877B56AAF991C34D0EA84EAF3716");
    let s = hex("F7CB1C942D657C41D436C7A1B6E29F65F3E900DBB9AFF4064DC4AB2F843ACDA8");
    let sig = der(&r, &s);
    check("P-256: RFC 6979's signature verifies", p256_verify(&pk, b"sample", &sig));
    check("…not for another message", !p256_verify(&pk, b"samplf", &sig));
    let mut bad = sig.clone();
    let k = bad.len() - 1;
    bad[k] ^= 1;
    check("…not with a changed signature", !p256_verify(&pk, b"sample", &bad));
    let mut pk2 = pk;
    pk2[63] ^= 1;
    check("…not with another key (not on the curve)", !p256_verify(&pk2, b"sample", &sig));
    let mut unc = [4u8; 65];
    unc[1..].copy_from_slice(&pk);
    check("an uncompressed point becomes x || y", p256_raw(&unc) == Some(pk));
    unc[64] ^= 1;
    check("…and an invalid one is refused", p256_raw(&unc).is_none());

    // Malformed DER: never accepted, never a crash.
    let mut seed = 12345u64;
    let mut ok = true;
    for _ in 0..10_000 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let mut d = sig.clone();
        match seed % 4 {
            0 => {
                let i = (seed >> 8) as usize % d.len();
                d[i] = (seed >> 20) as u8;
            }
            1 => d.truncate((seed >> 8) as usize % d.len()),
            2 => d.push((seed >> 8) as u8),
            _ => {
                let i = (seed >> 8) as usize % d.len();
                d.remove(i);
            }
        }
        if d != sig && p256_verify(&pk, b"sample", &d) {
            ok = false;
        }
    }
    check("10,000 malformed or changed signatures: none verifies", ok);
    check("DER: a zero-length or non-minimal integer is refused", der_rs(&[0x30, 6, 2, 1, 0, 2, 1, 1]).is_none() && der_rs(&[0x30, 7, 2, 2, 0, 1, 2, 1, 1]).is_none());
    println!("\n{fails} failure(s)");
    if fails > 0 {
        std::process::exit(1);
    }
}
