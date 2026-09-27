// Building HTML that is allowed markup (spec/markup.rs), proved (Verus,
// about this code): a Markup's bytes are always a sequence of allowed
// pieces, whatever the code that builds it does: new() makes a
// well-formed one (wf), every method keeps it well-formed, and the fields
// are private, so no other Markup can exist. The
// Markdown renderer (src/markdown.rs) builds with it, so the HTML it makes
// is allowed markup without proving anything about Markdown.
// The pieces are ghost: nothing of them exists when the program runs.
use vstd::prelude::*;
use crate::spec_markup::*;

verus! {

pub struct Markup {
    bytes: Vec<u8>,
    pieces: Ghost<Seq<Piece>>,
}

pub proof fn lemma_flatten_push(ps: Seq<Piece>, p: Piece)
    ensures flatten(ps.push(p)) == flatten(ps) + piece_bytes(p),
{
    assert(ps.push(p).drop_last() =~= ps);
}

impl Markup {
    pub closed spec fn wf(self) -> bool {
        &&& self.bytes@ == flatten(self.pieces@)
        &&& forall|i: int| 0 <= i < self.pieces@.len() ==> piece_ok(#[trigger] self.pieces@[i])
    }

    pub closed spec fn view(self) -> Seq<u8> {
        self.bytes@
    }

    pub fn new() -> (m: Markup)
        ensures m.view() == Seq::<u8>::empty(), m.wf(),
    {
        let m = Markup { bytes: Vec::new(), pieces: Ghost(Seq::empty()) };
        assert(flatten(Seq::<Piece>::empty()) =~= Seq::<u8>::empty());
        m
    }

    /// The bytes, and the law they obey.
    pub fn bytes(&self) -> (r: &Vec<u8>)
        requires self.wf(),
        ensures r@ == self.view(), markup_ok(r@),
    {
        proof {
            assert(flatten(self.pieces@) == self.bytes@);
        }
        &self.bytes
    }

    pub fn len(&self) -> (n: usize)
        ensures n == self.view().len(),
    {
        self.bytes.len()
    }

    fn put(&mut self, p: Ghost<Piece>, b: &[u8])
        requires old(self).wf(), piece_ok(p@), b@ == piece_bytes(p@),
        ensures final(self).wf(),
    {
        let ghost old_ps = self.pieces@;
        let mut i: usize = 0;
        let ghost start = self.bytes@;
        while i < b.len()
            invariant i <= b.len(), self.bytes@ == start + b@.subrange(0, i as int), self.pieces@ == old_ps,
            decreases b.len() - i,
        {
            self.bytes.push(b[i]);
            proof { assert(start + b@.subrange(0, i + 1) =~= (start + b@.subrange(0, i as int)).push(b@[i as int])); }
            i += 1;
        }
        proof {
            assert(b@.subrange(0, b@.len() as int) =~= b@);
            lemma_flatten_push(old_ps, p@);
            let ps = old_ps.push(p@);
            assert forall|k: int| 0 <= k < ps.len() implies piece_ok(#[trigger] ps[k]) by {
                if k < old_ps.len() { assert(ps[k] == old_ps[k]); }
            }
        }
        self.pieces = Ghost(old_ps.push(p@));
    }

    /// An allowed tag.
    pub fn tag(&mut self, t: Tag)
        requires old(self).wf(),
        ensures final(self).wf(),
    {
        match t {
            Tag::P => {
                let b: [u8; 3] = [60u8, 112, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::P_ => {
                let b: [u8; 4] = [60u8, 47, 112, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H2 => {
                let b: [u8; 4] = [60u8, 104, 50, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H2_ => {
                let b: [u8; 5] = [60u8, 47, 104, 50, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H3 => {
                let b: [u8; 4] = [60u8, 104, 51, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H3_ => {
                let b: [u8; 5] = [60u8, 47, 104, 51, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Strong => {
                let b: [u8; 8] = [60u8, 115, 116, 114, 111, 110, 103, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Strong_ => {
                let b: [u8; 9] = [60u8, 47, 115, 116, 114, 111, 110, 103, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Em => {
                let b: [u8; 4] = [60u8, 101, 109, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Em_ => {
                let b: [u8; 5] = [60u8, 47, 101, 109, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Code => {
                let b: [u8; 6] = [60u8, 99, 111, 100, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Code_ => {
                let b: [u8; 7] = [60u8, 47, 99, 111, 100, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Pre => {
                let b: [u8; 5] = [60u8, 112, 114, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Pre_ => {
                let b: [u8; 6] = [60u8, 47, 112, 114, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Quote => {
                let b: [u8; 12] = [60u8, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Quote_ => {
                let b: [u8; 13] = [60u8, 47, 98, 108, 111, 99, 107, 113, 117, 111, 116, 101, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Ul => {
                let b: [u8; 4] = [60u8, 117, 108, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Ul_ => {
                let b: [u8; 5] = [60u8, 47, 117, 108, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Ol => {
                let b: [u8; 4] = [60u8, 111, 108, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Ol_ => {
                let b: [u8; 5] = [60u8, 47, 111, 108, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Li => {
                let b: [u8; 4] = [60u8, 108, 105, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Li_ => {
                let b: [u8; 5] = [60u8, 47, 108, 105, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Br => {
                let b: [u8; 4] = [60u8, 98, 114, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Hr => {
                let b: [u8; 4] = [60u8, 104, 114, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::A_ => {
                let b: [u8; 4] = [60u8, 47, 97, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
        }
    }

    /// Text, escaped: each byte as itself, or as its entity.
    pub fn text(&mut self, s: &[u8])
        requires old(self).wf(),
        ensures final(self).wf(),
    {
        let mut i: usize = 0;
        while i < s.len()
            invariant self.wf(),
            decreases s.len() - i,
        {
            let c = s[i];
            if c == 60 {
                self.put(Ghost(Piece::Ent(Ent::Lt)), &[38, 108, 116, 59]);
            } else if c == 62 {
                self.put(Ghost(Piece::Ent(Ent::Gt)), &[38, 103, 116, 59]);
            } else if c == 38 {
                self.put(Ghost(Piece::Ent(Ent::Amp)), &[38, 97, 109, 112, 59]);
            } else if c == 34 {
                self.put(Ghost(Piece::Ent(Ent::Quot)), &[38, 113, 117, 111, 116, 59]);
            } else if c == 39 {
                self.put(Ghost(Piece::Ent(Ent::Apos)), &[38, 35, 51, 57, 59]);
            } else {
                let one = [c];
                assert(one@ =~= seq![c]);
                assert(text_ok(seq![c]));
                self.put(Ghost(Piece::Text(seq![c])), &one);
            }
            i += 1;
        }
    }

    /// <a href="url"> if url is allowed (true); nothing if not (false).
    pub fn link(&mut self, url: &[u8]) -> (ok: bool)
        requires old(self).wf(),
        ensures ok == url_ok(url@), final(self).wf(),
    {
        if !url_ok_exec(url) {
            return false;
        }
        let mut b: Vec<u8> = Vec::new();
        b.push(60); b.push(97); b.push(32); b.push(104); b.push(114); b.push(101); b.push(102); b.push(61); b.push(34);
        assert(b@ =~= link_start());
        let mut i: usize = 0;
        while i < url.len()
            invariant i <= url.len(), b@ == link_start() + url@.subrange(0, i as int), self.wf(),
            decreases url.len() - i,
        {
            b.push(url[i]);
            proof { assert(link_start() + url@.subrange(0, i + 1) =~= (link_start() + url@.subrange(0, i as int)).push(url@[i as int])); }
            i += 1;
        }
        b.push(34);
        b.push(62);
        proof {
            assert(url@.subrange(0, url@.len() as int) =~= url@);
            assert(b@ =~= link_start() + url@ + link_end());
        }
        self.put(Ghost(Piece::Link(url@)), b.as_slice());
        true
    }
}

pub fn url_ok_exec(u: &[u8]) -> (ok: bool)
    ensures ok == url_ok(u@),
{
    let https = u.len() >= 8 && u[0] == 104 && u[1] == 116 && u[2] == 116 && u[3] == 112 && u[4] == 115 && u[5] == 58 && u[6] == 47 && u[7] == 47;
    let http = u.len() >= 7 && u[0] == 104 && u[1] == 116 && u[2] == 116 && u[3] == 112 && u[4] == 58 && u[5] == 47 && u[6] == 47;
    let path = u.len() >= 1 && u[0] == 47 && (u.len() == 1 || u[1] != 47);
    proof {
        if u@.len() >= 8 {
            assert(https == (u@.subrange(0, 8) =~= seq![104u8, 116, 116, 112, 115, 58, 47, 47])) by {
                if u@.subrange(0, 8) =~= seq![104u8, 116, 116, 112, 115, 58, 47, 47] {
                    assert(u@[0] == u@.subrange(0, 8)[0]); assert(u@[1] == u@.subrange(0, 8)[1]);
                    assert(u@[2] == u@.subrange(0, 8)[2]); assert(u@[3] == u@.subrange(0, 8)[3]);
                    assert(u@[4] == u@.subrange(0, 8)[4]); assert(u@[5] == u@.subrange(0, 8)[5]);
                    assert(u@[6] == u@.subrange(0, 8)[6]); assert(u@[7] == u@.subrange(0, 8)[7]);
                }
            }
        }
        if u@.len() >= 7 {
            assert(http == (u@.subrange(0, 7) =~= seq![104u8, 116, 116, 112, 58, 47, 47])) by {
                if u@.subrange(0, 7) =~= seq![104u8, 116, 116, 112, 58, 47, 47] {
                    assert(u@[0] == u@.subrange(0, 7)[0]); assert(u@[1] == u@.subrange(0, 7)[1]);
                    assert(u@[2] == u@.subrange(0, 7)[2]); assert(u@[3] == u@.subrange(0, 7)[3]);
                    assert(u@[4] == u@.subrange(0, 7)[4]); assert(u@[5] == u@.subrange(0, 7)[5]);
                    assert(u@[6] == u@.subrange(0, 7)[6]);
                }
            }
        }
    }
    if !(https || http || path) {
        return false;
    }
    let mut i: usize = 0;
    while i < u.len()
        invariant i <= u.len(), forall|j: int| 0 <= j < i ==> 0x21 <= #[trigger] u@[j] <= 0x7e && plain(u@[j]) && u@[j] != 92,
        decreases u.len() - i,
    {
        let c = u[i];
        if !(0x21 <= c && c <= 0x7e && c != 60 && c != 62 && c != 34 && c != 39 && c != 38 && c != 92) {
            return false;
        }
        i += 1;
    }
    true
}

} // verus!
