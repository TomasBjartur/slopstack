// LAWS: HTTP/1.1 request heads (owned by people; a change here is a
// SECURITY DECISION). What the parser (src/http.rs) may accept, stated with
// predicates of our own over the bytes, not with the parser's code.
//
// A head is: METHOD SP TARGET SP "HTTP/1.1" CRLF, then header lines
// NAME ":" OWS VALUE OWS CRLF, then CRLF. (RFC 9112, narrowed: no obsolete
// line folding, no bare LF, HTTP/1.1 only, origin-form targets only.)
use vstd::prelude::*;

verus! {

/// RFC 9110 tchar: the bytes of a token (method names, header names).
pub open spec fn is_tchar(b: u8) -> bool {
    (48 <= b <= 57) || (97 <= b <= 122) || (65 <= b <= 90)
        || b == 33 || b == 35 || b == 36 || b == 37 || b == 38 || b == 39 || b == 42
        || b == 43 || b == 45 || b == 46 || b == 94 || b == 95 || b == 96 || b == 124 || b == 126
}

/// A byte of a target: visible ASCII (no space, no control, nothing above 0x7E).
pub open spec fn is_target_byte(b: u8) -> bool {
    0x21 <= b <= 0x7e
}

/// A byte of a header value: visible ASCII, space or tab (no controls, no
/// bytes above 0x7E: we refuse obs-text).
pub open spec fn is_value_byte(b: u8) -> bool {
    (0x21 <= b <= 0x7e) || b == 32 || b == 9
}

pub open spec fn is_ows(b: u8) -> bool {
    b == 32 || b == 9
}

/// The methods we answer.
pub open spec fn is_method(s: Seq<u8>) -> bool {
    s =~= seq![71u8, 69, 84] || s =~= seq![80u8, 79, 83, 84] || s =~= seq![72u8, 69, 65, 68]
}

/// " HTTP/1.1\r\n"
pub open spec fn version_crlf() -> Seq<u8> {
    seq![32u8, 72, 84, 84, 80, 47, 49, 46, 49, 13, 10]
}

/// The request line [0, line_end + 2): method, SP, target, " HTTP/1.1", CRLF.
pub open spec fn line_ok(s: Seq<u8>, method_end: int, target_start: int, target_end: int, line_end: int) -> bool {
    &&& 0 < method_end && target_start == method_end + 1 && target_start < target_end
    &&& line_end == target_end + 9 && line_end + 2 <= s.len()
    &&& is_method(s.subrange(0, method_end))
    &&& s[method_end] == 32
    &&& s[target_start] == 47
    &&& forall|i: int| target_start <= i < target_end ==> is_target_byte(#[trigger] s[i])
    &&& s.subrange(target_end, line_end + 2) =~= version_crlf()
}

/// A header line [ls, le + 2): NAME ":" OWS VALUE OWS CRLF, the value
/// [vs, ve) without spaces at either end.
pub open spec fn header_ok(s: Seq<u8>, ls: int, name_end: int, vs: int, ve: int, le: int) -> bool {
    &&& ls < name_end && name_end < vs && vs <= ve && ve <= le && le + 2 <= s.len()
    &&& forall|i: int| ls <= i < name_end ==> is_tchar(#[trigger] s[i])
    &&& s[name_end] == 58
    &&& forall|i: int| name_end < i < vs ==> is_ows(#[trigger] s[i])
    &&& forall|i: int| vs <= i < ve ==> is_value_byte(#[trigger] s[i])
    &&& forall|i: int| ve <= i < le ==> is_ows(#[trigger] s[i])
    &&& (vs < ve ==> !is_ows(s[vs]) && !is_ows(s[ve - 1]))
    &&& s[le] == 13 && s[le + 1] == 10
}

/// Where one header line is: [start, end + 2), its name [start, name_end),
/// its value [vs, ve).
#[derive(Clone, Copy)]
pub struct HeaderPos {
    pub start: usize,
    pub name_end: usize,
    pub vs: usize,
    pub ve: usize,
    pub end: usize,
}

/// Header k: a well-formed line, starting where the one before ended.
pub open spec fn header_at(s: Seq<u8>, hs: Seq<HeaderPos>, k: int, line_end: int) -> bool {
    &&& header_ok(s, hs[k].start as int, hs[k].name_end as int, hs[k].vs as int, hs[k].ve as int, hs[k].end as int)
    &&& hs[k].start == (if k == 0 { line_end + 2 } else { hs[k - 1].end + 2 })
}

/// THE LAW: a head the parser accepts is exactly this. The request line,
/// then the headers one after another, then CRLF; the target and the
/// head within their limits.
pub open spec fn head_ok(s: Seq<u8>, method_end: int, target_end: int, line_end: int, hs: Seq<HeaderPos>, len: int) -> bool {
    &&& line_ok(s, method_end, method_end + 1, target_end, line_end)
    &&& target_end - (method_end + 1) <= crate::limits::TARGET_MAX
    &&& hs.len() <= crate::limits::HEADERS_MAX
    &&& forall|k: int| 0 <= k < hs.len() ==> header_at(s, hs, k, line_end)
    &&& len == (if hs.len() == 0 { line_end } else { hs[hs.len() - 1].end as int }) + 4
    &&& s[len - 2] == 13 && s[len - 1] == 10
    &&& len <= s.len() && len <= crate::limits::HEAD_MAX
}

} // verus!
