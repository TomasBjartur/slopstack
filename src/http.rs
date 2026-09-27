// HTTP/1.1 request heads. Proved (Verus, about this code): every index is
// in bounds; a head is accepted only as spec/http.rs's head_ok says; "need
// more bytes" only while under HEAD_MAX (a client cannot hold a connection
// open by never finishing); every refusal is 400, 414, 431 or 501.
// No allocation: header positions go into the caller's Vec (cleared, its
// capacity kept from request to request).
use vstd::prelude::*;
use crate::limits::*;
use crate::spec_http::*;

verus! {

pub enum Parsed {
    /// A whole head of len bytes; hs holds its headers.
    Done { method_end: usize, target_end: usize, line_end: usize, len: usize },
    /// Not all of it yet (fewer than HEAD_MAX bytes so far).
    More,
    /// Refused, with this status.
    Bad(u16),
}

fn tchar(b: u8) -> (r: bool)
    ensures r == is_tchar(b),
{
    (48 <= b && b <= 57) || (97 <= b && b <= 122) || (65 <= b && b <= 90)
        || b == 33 || b == 35 || b == 36 || b == 37 || b == 38 || b == 39 || b == 42
        || b == 43 || b == 45 || b == 46 || b == 94 || b == 95 || b == 96 || b == 124 || b == 126
}

fn target_byte(b: u8) -> (r: bool)
    ensures r == is_target_byte(b),
{
    0x21 <= b && b <= 0x7e
}

fn value_byte(b: u8) -> (r: bool)
    ensures r == is_value_byte(b),
{
    (0x21 <= b && b <= 0x7e) || b == 32 || b == 9
}

fn ows(b: u8) -> (r: bool)
    ensures r == is_ows(b),
{
    b == 32 || b == 9
}

/// Out of bytes before the head ended: wait for more, unless the head is
/// already as long as it may be.
fn short(len: usize) -> (r: Parsed)
    ensures
        match r {
            Parsed::More => len < HEAD_MAX,
            Parsed::Bad(c) => c == 431,
            _ => false,
        },
{
    if len >= HEAD_MAX { Parsed::Bad(431) } else { Parsed::More }
}

fn method_known(buf: &[u8], end: usize) -> (r: bool)
    requires end <= buf.len(),
    ensures r == is_method(buf@.subrange(0, end as int)),
{
    if end == 3 && buf[0] == 71 && buf[1] == 69 && buf[2] == 84 {
        assert(buf@.subrange(0, 3) =~= seq![71u8, 69, 84]);
        true
    } else if end == 4 && buf[0] == 80 && buf[1] == 79 && buf[2] == 83 && buf[3] == 84 {
        assert(buf@.subrange(0, 4) =~= seq![80u8, 79, 83, 84]);
        true
    } else if end == 4 && buf[0] == 72 && buf[1] == 69 && buf[2] == 65 && buf[3] == 68 {
        assert(buf@.subrange(0, 4) =~= seq![72u8, 69, 65, 68]);
        true
    } else {
        proof {
            if end == 3 && is_method(buf@.subrange(0, 3)) {
                assert(buf@.subrange(0, 3)[0] == buf@[0]);
                assert(buf@.subrange(0, 3)[1] == buf@[1]);
                assert(buf@.subrange(0, 3)[2] == buf@[2]);
            }
            if end == 4 && is_method(buf@.subrange(0, 4)) {
                assert(buf@.subrange(0, 4)[0] == buf@[0]);
                assert(buf@.subrange(0, 4)[1] == buf@[1]);
                assert(buf@.subrange(0, 4)[2] == buf@[2]);
                assert(buf@.subrange(0, 4)[3] == buf@[3]);
            }
        }
        false
    }
}

/// " HTTP/1.1\r\n" at i?
fn version_at(buf: &[u8], i: usize) -> (r: bool)
    requires i + 11 <= buf.len(),
    ensures r == (buf@.subrange(i as int, i + 11) =~= version_crlf()),
{
    let v: [u8; 11] = [32, 72, 84, 84, 80, 47, 49, 46, 49, 13, 10];
    assert(v@ =~= version_crlf());
    let mut k: usize = 0;
    while k < 11
        invariant
            k <= 11, i + 11 <= buf.len(), v@ =~= version_crlf(),
            forall|j: int| 0 <= j < k ==> buf@[i + j] == v@[j],
        decreases 11 - k,
    {
        if buf[i + k] != v[k] {
            assert(buf@.subrange(i as int, i + 11)[k as int] != version_crlf()[k as int]);
            return false;
        }
        k += 1;
    }
    assert(buf@.subrange(i as int, i + 11) =~= version_crlf());
    true
}

/// Parses the head at the start of buf.
pub fn parse(buf: &[u8], hs: &mut Vec<HeaderPos>) -> (r: Parsed)
    ensures
        match r {
            Parsed::Done { method_end, target_end, line_end, len } =>
                head_ok(buf@, method_end as int, target_end as int, line_end as int, final(hs)@, len as int),
            Parsed::More => buf.len() < HEAD_MAX,
            Parsed::Bad(c) => c == 400 || c == 414 || c == 431 || c == 501,
        },
{
    hs.clear();
    let n: usize = if buf.len() < HEAD_MAX { buf.len() } else { HEAD_MAX };

    // The method: a token, then a space.
    let mut i: usize = 0;
    while i < n && tchar(buf[i])
        invariant i <= n, n <= buf.len(), n <= HEAD_MAX,
        decreases n - i,
    {
        i += 1;
    }
    if i == n {
        return short(buf.len());
    }
    if i == 0 || buf[i] != 32 {
        return Parsed::Bad(400);
    }
    let method_end = i;
    if !method_known(buf, method_end) {
        return Parsed::Bad(501);
    }

    // The target: visible bytes, starting with "/".
    let ts = i + 1;
    i = ts;
    while i < n && target_byte(buf[i])
        invariant
            ts <= i <= n, n <= buf.len(),
            forall|j: int| ts <= j < i ==> is_target_byte(#[trigger] buf@[j]),
        decreases n - i,
    {
        i += 1;
    }
    if i - ts > TARGET_MAX {
        return Parsed::Bad(414);
    }
    if i == n {
        return short(buf.len());
    }
    if i == ts || buf[ts] != 47 {
        return Parsed::Bad(400);
    }
    let target_end = i;

    // " HTTP/1.1" CRLF.
    if n - i < 11 {
        return short(buf.len());
    }
    if !version_at(buf, i) {
        return Parsed::Bad(400);
    }
    let line_end = i + 9;
    assert(line_ok(buf@, method_end as int, ts as int, target_end as int, line_end as int));

    // Headers, until an empty line.
    let mut p: usize = line_end + 2;
    loop
        invariant
            n <= buf.len(), n <= HEAD_MAX, line_end + 2 <= p <= n,
            line_ok(buf@, method_end as int, ts as int, target_end as int, line_end as int),
            ts == method_end + 1, target_end - ts <= TARGET_MAX,
            hs@.len() <= HEADERS_MAX,
            forall|k: int| 0 <= k < hs@.len() ==> header_at(buf@, hs@, k, line_end as int),
            p == (if hs@.len() == 0 { line_end + 2 } else { hs@[hs@.len() - 1].end + 2 }),
        decreases n - p,
    {
        if n - p < 2 {
            return short(buf.len());
        }
        if buf[p] == 13 {
            if buf[p + 1] == 10 {
                let len = p + 2;
                assert(head_ok(buf@, method_end as int, target_end as int, line_end as int, hs@, len as int));
                return Parsed::Done { method_end, target_end, line_end, len };
            }
            return Parsed::Bad(400);
        }
        if hs.len() == HEADERS_MAX {
            return Parsed::Bad(431);
        }
        // The name: a token, then ":".
        let ls = p;
        while p < n && tchar(buf[p])
            invariant
                ls <= p <= n, n <= buf.len(),
                forall|j: int| ls <= j < p ==> is_tchar(#[trigger] buf@[j]),
            decreases n - p,
        {
            p += 1;
        }
        if p == n {
            return short(buf.len());
        }
        if p == ls || buf[p] != 58 {
            return Parsed::Bad(400);
        }
        let name_end = p;
        p += 1;
        // Spaces before the value.
        while p < n && ows(buf[p])
            invariant
                name_end < p <= n, n <= buf.len(),
                forall|j: int| name_end < j < p ==> is_ows(#[trigger] buf@[j]),
            decreases n - p,
        {
            p += 1;
        }
        let vs = p;
        // The value, up to CR; ve: just after its last byte that is not a space.
        let mut ve = p;
        while p < n && value_byte(buf[p])
            invariant
                vs <= ve <= p <= n, n <= buf.len(),
                forall|j: int| vs <= j < p ==> is_value_byte(#[trigger] buf@[j]),
                forall|j: int| ve <= j < p ==> is_ows(#[trigger] buf@[j]),
                vs < ve ==> !is_ows(buf@[ve - 1]),
                vs < n ==> !is_ows(buf@[vs as int]),
            decreases n - p,
        {
            if !ows(buf[p]) {
                ve = p + 1;
            }
            p += 1;
        }
        if n - p < 2 {
            return short(buf.len());
        }
        if buf[p] != 13 || buf[p + 1] != 10 {
            return Parsed::Bad(400);
        }
        let h = HeaderPos { start: ls, name_end, vs, ve, end: p };
        assert(header_ok(buf@, ls as int, name_end as int, vs as int, ve as int, p as int));
        let ghost old_hs = hs@;
        hs.push(h);
        assert forall|k: int| 0 <= k < hs@.len() implies header_at(buf@, hs@, k, line_end as int) by {
            if k < old_hs.len() {
                assert(hs@[k] == old_hs[k]);
                if k > 0 {
                    assert(hs@[k - 1] == old_hs[k - 1]);
                }
                assert(header_at(buf@, old_hs, k, line_end as int));
            } else {
                assert(hs@[k] == h);
                if k > 0 {
                    assert(hs@[k - 1] == old_hs[k - 1]);
                }
            }
        }
        p = p + 2;
    }
}

} // verus!
