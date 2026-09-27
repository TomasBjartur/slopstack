// Building HTML that is allowed markup (spec/markup.rs), proved (Verus,
// about this code): a Markup's bytes are always a sequence of allowed
// pieces, whatever the code that builds it does: new() makes a
// well-formed one (wf), every method keeps it well-formed, and the fields
// are private, so no other Markup can exist. The CommonMark renderer
// (src/markdown.rs) builds with it, so the HTML it makes is allowed markup
// without proving anything about Markdown. The pieces are ghost: nothing
// of them exists when the program runs.
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

/// v gets s appended.
fn append(v: &mut Vec<u8>, s: &[u8])
    ensures final(v)@ == old(v)@ + s@,
{
    let ghost start = v@;
    let mut i: usize = 0;
    while i < s.len()
        invariant i <= s.len(), v@ == start + s@.subrange(0, i as int),
        decreases s.len() - i,
    {
        v.push(s[i]);
        proof { assert(start + s@.subrange(0, i + 1) =~= (start + s@.subrange(0, i as int)).push(s@[i as int])); }
        i += 1;
    }
    proof { assert(s@.subrange(0, s@.len() as int) =~= s@); }
}

/// Text escaped: < > " & as entities, the rest as it is.
pub fn escape(s: &[u8]) -> (e: Vec<u8>)
    ensures text_ok(e@),
{
    let mut e: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < s.len()
        invariant text_ok(e@),
        decreases s.len() - i,
    {
        let c = s[i];
        let ghost before = e@;
        if c == 60 {
            append(&mut e, &[38, 108, 116, 59]);
        } else if c == 62 {
            append(&mut e, &[38, 103, 116, 59]);
        } else if c == 34 {
            append(&mut e, &[38, 113, 117, 111, 116, 59]);
        } else if c == 38 {
            append(&mut e, &[38, 97, 109, 112, 59]);
        } else {
            e.push(c);
        }
        proof {
            assert forall|k: int| 0 <= k < e@.len() implies #[trigger] e@[k] != 60 && e@[k] != 62 && e@[k] != 34 by {
                if k < before.len() { assert(e@[k] == before[k]); }
            }
        }
        i += 1;
    }
    e
}

fn is_alpha_exec(b: u8) -> (r: bool)
    ensures r == is_alpha(b),
{
    (65 <= b && b <= 90) || (97 <= b && b <= 122)
}

fn scheme_char_exec(b: u8) -> (r: bool)
    ensures r == scheme_char(b),
{
    is_alpha_exec(b) || (48 <= b && b <= 57) || b == 43 || b == 45 || b == 46
}

/// u[0..n], lowercased, is w?
fn lower_is(u: &[u8], n: usize, w: &[u8]) -> (r: bool)
    requires n <= u.len(),
    ensures r == (u@.subrange(0, n as int).map_values(|b: u8| lower(b)) =~= w@),
{
    let ghost l = u@.subrange(0, n as int).map_values(|b: u8| lower(b));
    if w.len() != n {
        proof { assert(l.len() == n); }
        return false;
    }
    let mut i: usize = 0;
    while i < n
        invariant n <= u.len(), w.len() == n, i <= n, l == u@.subrange(0, n as int).map_values(|b: u8| lower(b)),
            forall|j: int| 0 <= j < i ==> l[j] == w@[j],
        decreases n - i,
    {
        let c = u[i];
        let lc = if 65 <= c && c <= 90 { c + 32 } else { c };
        if lc != w[i] {
            proof { assert(l[i as int] != w@[i as int]); }
            return false;
        }
        i += 1;
    }
    proof { assert(l =~= w@); }
    true
}

fn scheme_allowed_exec(u: &[u8], n: usize) -> (r: bool)
    requires n <= u.len(),
    ensures r == scheme_allowed(u@.subrange(0, n as int)),
{
    let h: [u8; 4] = [104, 116, 116, 112];
    let hs: [u8; 5] = [104, 116, 116, 112, 115];
    let m: [u8; 6] = [109, 97, 105, 108, 116, 111];
    assert(h@ =~= seq![104u8, 116, 116, 112]);
    assert(hs@ =~= seq![104u8, 116, 116, 112, 115]);
    assert(m@ =~= seq![109u8, 97, 105, 108, 116, 111]);
    lower_is(u, n, &h) || lower_is(u, n, &hs) || lower_is(u, n, &m)
}

/// The URL law (spec/markup.rs url_ok).
pub fn url_ok_exec(u: &[u8]) -> (ok: bool)
    ensures ok == url_ok(u@),
{
    let mut i: usize = 0;
    while i < u.len()
        invariant i <= u.len(), forall|j: int| 0 <= j < i ==> url_byte(#[trigger] u@[j]),
        decreases u.len() - i,
    {
        let c = u[i];
        if !(0x21 <= c && c <= 0x7e && c != 60 && c != 62 && c != 34 && c != 92 && c != 96) {
            return false;
        }
        i += 1;
    }
    // The scheme, as browsers find it: the first byte that is not a scheme
    // character; if it is ':' and the first byte is a letter, a scheme.
    let mut k: usize = 0;
    while k < u.len() && scheme_char_exec(u[k])
        invariant k <= u.len(), forall|j: int| 0 <= j < k ==> scheme_char(#[trigger] u@[j]),
        decreases u.len() - k,
    {
        k += 1;
    }
    proof {
        // Any i with scheme_at(u, i) is k: before k every byte is a scheme
        // character (not ':'), and u[k] is not one.
        assert forall|i: int| scheme_at(u@, i) implies i == k by {
            if i < k { assert(scheme_char(u@[i])); }
            if i > k { assert(scheme_char(u@[k as int])); }
        }
    }
    if k > 0 && k < u.len() && u[k] == 58 && is_alpha_exec(u[0]) {
        proof { assert(scheme_at(u@, k as int)); }
        scheme_allowed_exec(u, k)
    } else {
        proof {
            assert forall|i: int| scheme_at(u@, i) implies scheme_allowed(u@.subrange(0, i)) by {
                assert(i == k);
            }
        }
        true
    }
}

/// The URL for an attribute: every & as &amp; (spec url_attr).
fn url_attr_exec(u: &[u8]) -> (v: Vec<u8>)
    ensures v@ == url_attr(u@),
{
    let mut v: Vec<u8> = Vec::new();
    let mut i: usize = 0;
    while i < u.len()
        invariant i <= u.len(), v@ == url_attr(u@.subrange(0, i as int)),
        decreases u.len() - i,
    {
        let ghost prev = v@;
        if u[i] == 38 {
            append(&mut v, &[38, 97, 109, 112, 59]);
        } else {
            v.push(u[i]);
        }
        proof {
            let s1 = u@.subrange(0, i + 1);
            assert(s1.drop_last() =~= u@.subrange(0, i as int));
            assert(s1.last() == u@[i as int]);
            if u@[i as int] == 38 {
                assert(v@ =~= prev + seq![38u8, 97, 109, 112, 59]);
            } else {
                assert(v@ =~= prev + seq![u@[i as int]]);
            }
        }
        i += 1;
    }
    proof { assert(u@.subrange(0, u@.len() as int) =~= u@); }
    v
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
        ensures m.wf(),
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
        append(&mut self.bytes, b);
        proof {
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
            Tag::H1 => {
                let b: [u8; 4] = [60u8, 104, 49, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H1_ => {
                let b: [u8; 5] = [60u8, 47, 104, 49, 62];
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
            Tag::H4 => {
                let b: [u8; 4] = [60u8, 104, 52, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H4_ => {
                let b: [u8; 5] = [60u8, 47, 104, 52, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H5 => {
                let b: [u8; 4] = [60u8, 104, 53, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H5_ => {
                let b: [u8; 5] = [60u8, 47, 104, 53, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H6 => {
                let b: [u8; 4] = [60u8, 104, 54, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::H6_ => {
                let b: [u8; 5] = [60u8, 47, 104, 54, 62];
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
                let b: [u8; 6] = [60u8, 98, 114, 32, 47, 62];
                assert(b@ =~= tag_bytes(t));
                self.put(Ghost(Piece::Tag(t)), &b);
            }
            Tag::Hr => {
                let b: [u8; 6] = [60u8, 104, 114, 32, 47, 62];
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

    /// Text, escaped.
    pub fn text(&mut self, s: &[u8])
        requires old(self).wf(),
        ensures final(self).wf(),
    {
        let e = escape(s);
        self.put(Ghost(Piece::Text(e@)), e.as_slice());
    }

    /// <a href="url" title="title"> if url is allowed (true); else nothing.
    pub fn link(&mut self, url: &[u8], title: Option<&[u8]>) -> (ok: bool)
        requires old(self).wf(),
        ensures ok == url_ok(url@), final(self).wf(),
    {
        if !url_ok_exec(url) {
            return false;
        }
        let t = match title { Some(x) => Some(escape(x)), None => None };
        let ghost tv: Option<Seq<u8>> = match &t { Some(x) => Some(x@), None => None };
        let mut b: Vec<u8> = Vec::new();
        append(&mut b, &[60, 97, 32, 104, 114, 101, 102, 61, 34]);
        let ua = url_attr_exec(url);
        append(&mut b, ua.as_slice());
        let ghost before_title = b@;
        match &t {
            Some(x) => {
                append(&mut b, &[34, 32, 116, 105, 116, 108, 101, 61, 34]);
                append(&mut b, x.as_slice());
            }
            None => {}
        }
        proof {
            assert(b@ =~= before_title + title_part(tv));
        }
        append(&mut b, &[34, 62]);
        proof {
            assert(b@ =~= seq![60u8, 97, 32, 104, 114, 101, 102, 61, 34] + url_attr(url@) + title_part(tv) + seq![34u8, 62]);
        }
        self.put(Ghost(Piece::Link(url@, tv)), b.as_slice());
        true
    }

    /// <img src="url" alt="alt" title="title" /> if url is allowed (true).
    pub fn img(&mut self, url: &[u8], alt: &[u8], title: Option<&[u8]>) -> (ok: bool)
        requires old(self).wf(),
        ensures ok == url_ok(url@), final(self).wf(),
    {
        if !url_ok_exec(url) {
            return false;
        }
        let a = escape(alt);
        let t = match title { Some(x) => Some(escape(x)), None => None };
        let ghost tv: Option<Seq<u8>> = match &t { Some(x) => Some(x@), None => None };
        let mut b: Vec<u8> = Vec::new();
        append(&mut b, &[60, 105, 109, 103, 32, 115, 114, 99, 61, 34]);
        let ua = url_attr_exec(url);
        append(&mut b, ua.as_slice());
        append(&mut b, &[34, 32, 97, 108, 116, 61, 34]);
        append(&mut b, a.as_slice());
        let ghost before_title = b@;
        match &t {
            Some(x) => {
                append(&mut b, &[34, 32, 116, 105, 116, 108, 101, 61, 34]);
                append(&mut b, x.as_slice());
            }
            None => {}
        }
        proof { assert(b@ =~= before_title + title_part(tv)); }
        append(&mut b, &[34, 32, 47, 62]);
        proof {
            assert(b@ =~= seq![60u8, 105, 109, 103, 32, 115, 114, 99, 61, 34] + url_attr(url@)
                + seq![34u8, 32, 97, 108, 116, 61, 34] + a@ + title_part(tv) + seq![34u8, 32, 47, 62]);
        }
        self.put(Ghost(Piece::Img(url@, a@, tv)), b.as_slice());
        true
    }

    /// <code class="language-lang"> (lang escaped).
    pub fn code_lang(&mut self, lang: &[u8])
        requires old(self).wf(),
        ensures final(self).wf(),
    {
        let l = escape(lang);
        let mut b: Vec<u8> = Vec::new();
        append(&mut b, &[60, 99, 111, 100, 101, 32, 99, 108, 97, 115, 115, 61, 34, 108, 97, 110, 103, 117, 97, 103, 101, 45]);
        append(&mut b, l.as_slice());
        append(&mut b, &[34, 62]);
        proof {
            assert(b@ =~= seq![60u8, 99, 111, 100, 101, 32, 99, 108, 97, 115, 115, 61, 34, 108, 97, 110, 103, 117, 97, 103, 101, 45] + l@ + seq![34u8, 62]);
        }
        self.put(Ghost(Piece::CodeLang(l@)), b.as_slice());
    }

    /// <ol start="n">
    pub fn ol_start(&mut self, n: u64)
        requires old(self).wf(),
        ensures final(self).wf(),
    {
        // The digits of n, most significant first (at most 20).
        let mut d: Vec<u8> = Vec::new();
        let mut v = n;
        let mut k: usize = 0;
        while k < 20 && (k == 0 || v > 0)
            invariant k <= 20, d@.len() == k, forall|i: int| 0 <= i < d@.len() ==> 48 <= #[trigger] d@[i] <= 57,
            decreases 20 - k,
        {
            let ghost before = d@;
            d.insert(0, (48 + v % 10) as u8);
            proof {
                assert forall|i: int| 0 <= i < d@.len() implies 48 <= #[trigger] d@[i] <= 57 by {
                    if i > 0 { assert(d@[i] == before[i - 1]); }
                }
            }
            v = v / 10;
            k += 1;
        }
        let mut b: Vec<u8> = Vec::new();
        append(&mut b, &[60, 111, 108, 32, 115, 116, 97, 114, 116, 61, 34]);
        append(&mut b, d.as_slice());
        append(&mut b, &[34, 62]);
        proof {
            assert(b@ =~= seq![60u8, 111, 108, 32, 115, 116, 97, 114, 116, 61, 34] + d@ + seq![34u8, 62]);
            assert(digits(d@));
        }
        self.put(Ghost(Piece::OlStart(d@)), b.as_slice());
    }
}

} // verus!
