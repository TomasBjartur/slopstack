// LAWS: what HTML we store and show for users' text (owned by people; a
// change here is a SECURITY DECISION). No script can come from a writer's
// text: rendered HTML is a sequence of pieces, each of them
//   - an allowed tag (a fixed list, no attributes);
//   - a link's opening <a href="URL"> with a URL that is http(s) or a path
//     on this site, of bytes that cannot end the attribute;
//   - an entity (&amp; &lt; &gt; &quot; &#39;);
//   - text with no < > " ' & at all.
// So no element outside the list, no attribute but a checked href, no
// javascript: URL, and no way out of a text or an attribute.
use vstd::prelude::*;

verus! {

pub enum Tag {
    P, P_, H2, H2_, H3, H3_, Strong, Strong_, Em, Em_, Code, Code_, Pre, Pre_,
    Quote, Quote_, Ul, Ul_, Ol, Ol_, Li, Li_, Br, Hr, A_,
}

pub enum Ent { Amp, Lt, Gt, Quot, Apos }

pub enum Piece {
    Tag(Tag),
    Ent(Ent),
    Text(Seq<u8>),
    Link(Seq<u8>),
}

pub open spec fn tag_bytes(t: Tag) -> Seq<u8> {
    match t {
        Tag::P => seq![60u8, 112, 62],                               // <p>
        Tag::P_ => seq![60u8, 47, 112, 62],                          // </p>
        Tag::H2 => seq![60u8, 104, 50, 62],                          // <h2>
        Tag::H2_ => seq![60u8, 47, 104, 50, 62],                     // </h2>
        Tag::H3 => seq![60u8, 104, 51, 62],
        Tag::H3_ => seq![60u8, 47, 104, 51, 62],
        Tag::Strong => seq![60u8, 115, 116, 114, 111, 110, 103, 62], // <strong>
        Tag::Strong_ => seq![60u8, 47, 115, 116, 114, 111, 110, 103, 62],
        Tag::Em => seq![60u8, 101, 109, 62],                         // <em>
        Tag::Em_ => seq![60u8, 47, 101, 109, 62],
        Tag::Code => seq![60u8, 99, 111, 100, 101, 62],              // <code>
        Tag::Code_ => seq![60u8, 47, 99, 111, 100, 101, 62],
        Tag::Pre => seq![60u8, 112, 114, 101, 62],                   // <pre>
        Tag::Pre_ => seq![60u8, 47, 112, 114, 101, 62],
        Tag::Quote => seq![60u8, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62],     // <blockquote>
        Tag::Quote_ => seq![60u8, 47, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62],
        Tag::Ul => seq![60u8, 117, 108, 62],                         // <ul>
        Tag::Ul_ => seq![60u8, 47, 117, 108, 62],
        Tag::Ol => seq![60u8, 111, 108, 62],                         // <ol>
        Tag::Ol_ => seq![60u8, 47, 111, 108, 62],
        Tag::Li => seq![60u8, 108, 105, 62],                         // <li>
        Tag::Li_ => seq![60u8, 47, 108, 105, 62],
        Tag::Br => seq![60u8, 98, 114, 62],                          // <br>
        Tag::Hr => seq![60u8, 104, 114, 62],                         // <hr>
        Tag::A_ => seq![60u8, 47, 97, 62],                           // </a>
    }
}

pub open spec fn ent_bytes(e: Ent) -> Seq<u8> {
    match e {
        Ent::Amp => seq![38u8, 97, 109, 112, 59],        // &amp;
        Ent::Lt => seq![38u8, 108, 116, 59],             // &lt;
        Ent::Gt => seq![38u8, 103, 116, 59],             // &gt;
        Ent::Quot => seq![38u8, 113, 117, 111, 116, 59], // &quot;
        Ent::Apos => seq![38u8, 35, 51, 57, 59],         // &#39;
    }
}

/// A byte that may stand as itself in text or in an attribute value.
pub open spec fn plain(b: u8) -> bool {
    b != 60 && b != 62 && b != 34 && b != 39 && b != 38
}

pub open spec fn text_ok(t: Seq<u8>) -> bool {
    forall|i: int| 0 <= i < t.len() ==> plain(#[trigger] t[i])
}

/// A link's URL: "https://", "http://" or "/" (not "//") first; then only
/// visible ASCII that is plain (no quote, no < >, no &), no backslash.
pub open spec fn url_ok(u: Seq<u8>) -> bool {
    &&& (u.len() >= 8 && u.subrange(0, 8) =~= seq![104u8, 116, 116, 112, 115, 58, 47, 47]
        || u.len() >= 7 && u.subrange(0, 7) =~= seq![104u8, 116, 116, 112, 58, 47, 47]
        || u.len() >= 1 && u[0] == 47 && (u.len() == 1 || u[1] != 47))
    &&& forall|i: int| 0 <= i < u.len() ==> 0x21 <= #[trigger] u[i] <= 0x7e && plain(u[i]) && u[i] != 92
}

/// <a href="
pub open spec fn link_start() -> Seq<u8> {
    seq![60u8, 97, 32, 104, 114, 101, 102, 61, 34]
}

/// ">
pub open spec fn link_end() -> Seq<u8> {
    seq![34u8, 62]
}

pub open spec fn piece_ok(p: Piece) -> bool {
    match p {
        Piece::Text(t) => text_ok(t),
        Piece::Link(u) => url_ok(u),
        _ => true,
    }
}

pub open spec fn piece_bytes(p: Piece) -> Seq<u8> {
    match p {
        Piece::Tag(t) => tag_bytes(t),
        Piece::Ent(e) => ent_bytes(e),
        Piece::Text(t) => t,
        Piece::Link(u) => link_start() + u + link_end(),
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
