// LAWS: what HTML we show for users' text (owned by people; a change here
// is a SECURITY DECISION). No script can come from a writer's text.
// Rendered HTML is a sequence of pieces, each of them:
//   - an allowed tag, with no attributes (a fixed list);
//   - text with no < > " (so it can open no tag, and end no attribute);
//   - a link or an image, whose URL has only visible ASCII (no space, no
//     control, none of < > " \ `), is written with every & as &amp; (so
//     the browser sees exactly the URL checked: no character references
//     inside it), and has no scheme, or http, https or mailto (so no
//     javascript:, data:, vbscript: ...). The scheme is found as browsers
//     find it: a letter, then letters, digits, + - . up to a ':'.
//     Its title and alt are text (as above), inside double quotes;
//   - a code block's language class, as text (as above);
//   - a numbered list's start, digits only.
// This is CommonMark's HTML without raw HTML, and without dangerous URLs.
use vstd::prelude::*;

verus! {

pub enum Tag {
    P, P_, H1, H1_, H2, H2_, H3, H3_, H4, H4_, H5, H5_, H6, H6_,
    Strong, Strong_, Em, Em_, Code, Code_, Pre, Pre_, Quote, Quote_,
    Ul, Ul_, Ol, Ol_, Li, Li_, Br, Hr, A_,
}

pub enum Piece {
    Tag(Tag),
    Text(Seq<u8>),
    Link(Seq<u8>, Option<Seq<u8>>),
    Img(Seq<u8>, Seq<u8>, Option<Seq<u8>>),
    CodeLang(Seq<u8>),
    OlStart(Seq<u8>),
}

pub open spec fn tag_bytes(t: Tag) -> Seq<u8> {
    match t {
        Tag::P => seq![60u8, 112, 62],
        Tag::P_ => seq![60u8, 47, 112, 62],
        Tag::H1 => seq![60u8, 104, 49, 62],
        Tag::H1_ => seq![60u8, 47, 104, 49, 62],
        Tag::H2 => seq![60u8, 104, 50, 62],
        Tag::H2_ => seq![60u8, 47, 104, 50, 62],
        Tag::H3 => seq![60u8, 104, 51, 62],
        Tag::H3_ => seq![60u8, 47, 104, 51, 62],
        Tag::H4 => seq![60u8, 104, 52, 62],
        Tag::H4_ => seq![60u8, 47, 104, 52, 62],
        Tag::H5 => seq![60u8, 104, 53, 62],
        Tag::H5_ => seq![60u8, 47, 104, 53, 62],
        Tag::H6 => seq![60u8, 104, 54, 62],
        Tag::H6_ => seq![60u8, 47, 104, 54, 62],
        Tag::Strong => seq![60u8, 115, 116, 114, 111, 110, 103, 62],
        Tag::Strong_ => seq![60u8, 47, 115, 116, 114, 111, 110, 103, 62],
        Tag::Em => seq![60u8, 101, 109, 62],
        Tag::Em_ => seq![60u8, 47, 101, 109, 62],
        Tag::Code => seq![60u8, 99, 111, 100, 101, 62],
        Tag::Code_ => seq![60u8, 47, 99, 111, 100, 101, 62],
        Tag::Pre => seq![60u8, 112, 114, 101, 62],
        Tag::Pre_ => seq![60u8, 47, 112, 114, 101, 62],
        Tag::Quote => seq![60u8, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62],
        Tag::Quote_ => seq![60u8, 47, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62],
        Tag::Ul => seq![60u8, 117, 108, 62],
        Tag::Ul_ => seq![60u8, 47, 117, 108, 62],
        Tag::Ol => seq![60u8, 111, 108, 62],
        Tag::Ol_ => seq![60u8, 47, 111, 108, 62],
        Tag::Li => seq![60u8, 108, 105, 62],
        Tag::Li_ => seq![60u8, 47, 108, 105, 62],
        Tag::Br => seq![60u8, 98, 114, 32, 47, 62],             // <br />
        Tag::Hr => seq![60u8, 104, 114, 32, 47, 62],            // <hr />
        Tag::A_ => seq![60u8, 47, 97, 62],
    }
}

/// Text (and attribute values, always in double quotes): no < > ".
pub open spec fn text_ok(t: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < t.len() ==> #[trigger] t[i] != 60 && t[i] != 62 && t[i] != 34
}

pub open spec fn is_alpha(b: u8) -> bool {
    (65 <= b <= 90) || (97 <= b <= 122)
}

pub open spec fn scheme_char(b: u8) -> bool {
    is_alpha(b) || (48 <= b <= 57) || b == 43 || b == 45 || b == 46
}

pub open spec fn lower(b: u8) -> u8 {
    if 65 <= b <= 90 { (b + 32) as u8 } else { b }
}

/// The scheme s (lowercase) is one we allow: http, https, mailto.
pub open spec fn scheme_allowed(s: Seq<u8>) -> bool {
    let l = s.map_values(|b: u8| lower(b));
    l =~= seq![104u8, 116, 116, 112] || l =~= seq![104u8, 116, 116, 112, 115] || l =~= seq![109u8, 97, 105, 108, 116, 111]
}

/// u[0..i] is a scheme and u[i] its ':'.
pub open spec fn scheme_at(u: Seq<u8>, i: int) -> bool {
    &&& 0 < i < u.len()
    &&& u[i] == 58
    &&& is_alpha(u[0])
    &&& forall|j: int| 0 <= j < i ==> scheme_char(#[trigger] u[j])
}

pub open spec fn url_byte(b: u8) -> bool {
    0x21 <= b <= 0x7e && b != 60 && b != 62 && b != 34 && b != 92 && b != 96
}

pub open spec fn url_ok(u: Seq<u8>) -> bool {
    &&& forall|i: int| 0 <= i < u.len() ==> url_byte(#[trigger] u[i])
    &&& forall|i: int| scheme_at(u, i) ==> scheme_allowed(u.subrange(0, i))
}

/// A URL as written into an attribute: every & as &amp;.
pub open spec fn url_attr(u: Seq<u8>) -> Seq<u8>
    decreases u.len(),
{
    if u.len() == 0 {
        Seq::empty()
    } else {
        url_attr(u.drop_last()) + if u.last() == 38 { seq![38u8, 97, 109, 112, 59] } else { seq![u.last()] }
    }
}

pub open spec fn digits(d: Seq<u8>) -> bool {
    d.len() > 0 && forall|i: int| 0 <= i < d.len() ==> 48 <= #[trigger] d[i] <= 57
}

/// " title="t  (or nothing)
pub open spec fn title_part(t: Option<Seq<u8>>) -> Seq<u8> {
    match t {
        Some(x) => seq![34u8, 32, 116, 105, 116, 108, 101, 61, 34] + x,
        None => Seq::empty(),
    }
}

pub open spec fn piece_ok(p: Piece) -> bool {
    match p {
        Piece::Tag(_) => true,
        Piece::Text(t) => text_ok(t),
        Piece::Link(u, t) => url_ok(u) && (t matches Some(x) ==> text_ok(x)),
        Piece::Img(u, alt, t) => url_ok(u) && text_ok(alt) && (t matches Some(x) ==> text_ok(x)),
        Piece::CodeLang(l) => text_ok(l),
        Piece::OlStart(d) => digits(d),
    }
}

pub open spec fn piece_bytes(p: Piece) -> Seq<u8> {
    match p {
        Piece::Tag(t) => tag_bytes(t),
        Piece::Text(t) => t,
        // <a href="URL" title="T">
        Piece::Link(u, t) => seq![60u8, 97, 32, 104, 114, 101, 102, 61, 34] + url_attr(u) + title_part(t) + seq![34u8, 62],
        // <img src="URL" alt="A" title="T" />
        Piece::Img(u, alt, t) => seq![60u8, 105, 109, 103, 32, 115, 114, 99, 61, 34] + url_attr(u)
            + seq![34u8, 32, 97, 108, 116, 61, 34] + alt + title_part(t) + seq![34u8, 32, 47, 62],
        // <code class="language-L">
        Piece::CodeLang(l) => seq![60u8, 99, 111, 100, 101, 32, 99, 108, 97, 115, 115, 61, 34, 108, 97, 110, 103, 117, 97, 103, 101, 45] + l + seq![34u8, 62],
        // <ol start="D">
        Piece::OlStart(d) => seq![60u8, 111, 108, 32, 115, 116, 97, 114, 116, 61, 34] + d + seq![34u8, 62],
    }
}

pub open spec fn flatten(ps: Seq<Piece>) -> Seq<u8>
    decreases ps.len(),
{
    if ps.len() == 0 { Seq::empty() } else { flatten(ps.drop_last()) + piece_bytes(ps.last()) }
}

/// THE LAW: html is allowed markup.
pub open spec fn markup_ok(html: Seq<u8>) -> bool {
    exists|ps: Seq<Piece>| #[trigger] flatten(ps) == html && forall|i: int| 0 <= i < ps.len() ==> piece_ok(#[trigger] ps[i])
}

} // verus!
