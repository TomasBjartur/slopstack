// Markdown (a small, predictable subset) to HTML, through html::Markup.
// Safety does not depend on this file: Markup can only hold allowed markup
// (proved, src/html.rs); bugs here can only mis-format a post.
//
// Blocks: paragraphs (blank-line separated), "# "/"## " (h2) and "### "
// (h3) headings, "- "/"* " lists, "1. " numbered lists, "> " quotes
// (nested at most QUOTE_DEPTH), ``` fenced code. Inline: **strong**, *em*
// or _em_, `code`, [text](url) (url must be allowed, else the brackets
// stay as text), and \x for a literal x. Every search for a closing
// marker is bounded (WINDOW bytes; more for link text and URLs), and inline
// nesting is bounded, so rendering is linear in the input.
use crate::html::Markup;
use crate::spec_markup::Tag;

const WINDOW: usize = 128;
const LINK_TEXT: usize = 256;
const URL_MAX: usize = 512;
const INLINE_DEPTH: u32 = 4;
const QUOTE_DEPTH: u32 = 8;

pub fn render(md: &[u8]) -> Markup {
    let mut m = Markup::new();
    blocks(md, &mut m, 0);
    m
}

/// The lines of s, without their line breaks (a CR before LF is dropped).
fn lines(s: &[u8]) -> impl Iterator<Item = &[u8]> {
    s.split(|&b| b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l))
}

fn is_blank(l: &[u8]) -> bool {
    l.iter().all(|&b| b == b' ' || b == b'\t')
}

fn heading(l: &[u8]) -> Option<(Tag, Tag, &[u8])> {
    if let Some(t) = l.strip_prefix(b"### ") {
        Some((Tag::H3, Tag::H3_, t))
    } else if let Some(t) = l.strip_prefix(b"## ").or_else(|| l.strip_prefix(b"# ")) {
        Some((Tag::H2, Tag::H2_, t))
    } else {
        None
    }
}

fn bullet(l: &[u8]) -> Option<&[u8]> {
    l.strip_prefix(b"- ").or_else(|| l.strip_prefix(b"* "))
}

/// "12. item" -> "item"
fn numbered(l: &[u8]) -> Option<&[u8]> {
    let d = l.iter().take(9).take_while(|b| b.is_ascii_digit()).count();
    if d > 0 && l.len() > d + 1 && l[d] == b'.' && l[d + 1] == b' ' { Some(&l[d + 2..]) } else { None }
}

fn quote(l: &[u8]) -> Option<&[u8]> {
    l.strip_prefix(b"> ").or_else(|| l.strip_prefix(b">"))
}

fn fence(l: &[u8]) -> bool {
    l.starts_with(b"```")
}

/// A line that starts a block of its own (ends a paragraph).
fn starts_block(l: &[u8]) -> bool {
    heading(l).is_some() || bullet(l).is_some() || numbered(l).is_some() || quote(l).is_some() || fence(l)
}

fn blocks(s: &[u8], m: &mut Markup, depth: u32) {
    let ls: Vec<&[u8]> = lines(s).collect();
    let mut i = 0;
    while i < ls.len() {
        let l = ls[i];
        if is_blank(l) {
            i += 1;
        } else if fence(l) {
            // Up to the closing fence (or the end), as it is.
            m.tag(Tag::Pre);
            m.tag(Tag::Code);
            i += 1;
            let mut first = true;
            while i < ls.len() && !fence(ls[i]) {
                if !first {
                    m.text(b"\n");
                }
                m.text(ls[i]);
                first = false;
                i += 1;
            }
            i += 1; // (the closing fence)
            m.tag(Tag::Code_);
            m.tag(Tag::Pre_);
        } else if let Some((open, close, t)) = heading(l) {
            m.tag(open);
            inline(t, m, 0);
            m.tag(close);
            i += 1;
        } else if bullet(l).is_some() || numbered(l).is_some() {
            let ordered = numbered(l).is_some();
            m.tag(if ordered { Tag::Ol } else { Tag::Ul });
            while i < ls.len() {
                let item = if ordered { numbered(ls[i]) } else { bullet(ls[i]) };
                let Some(t) = item else { break };
                m.tag(Tag::Li);
                inline(t, m, 0);
                m.tag(Tag::Li_);
                i += 1;
            }
            m.tag(if ordered { Tag::Ol_ } else { Tag::Ul_ });
        } else if quote(l).is_some() && depth < QUOTE_DEPTH {
            // The quoted lines, without their markers, as blocks of their own.
            let mut inner: Vec<u8> = Vec::new();
            while i < ls.len() {
                let Some(t) = quote(ls[i]) else { break };
                inner.extend_from_slice(t);
                inner.push(b'\n');
                i += 1;
            }
            m.tag(Tag::Quote);
            blocks(&inner, m, depth + 1);
            m.tag(Tag::Quote_);
        } else {
            // A paragraph: lines up to a blank line or a block's start.
            m.tag(Tag::P);
            let mut first = true;
            while i < ls.len() && !is_blank(ls[i]) && (first || !starts_block(ls[i])) {
                if !first {
                    m.text(b"\n");
                }
                inline(ls[i], m, 0);
                first = false;
                i += 1;
            }
            m.tag(Tag::P_);
        }
    }
}

/// Where the first `pat` is in s[from..from + window], if at all.
fn find(s: &[u8], from: usize, pat: &[u8], window: usize) -> Option<usize> {
    let end = (from + window + pat.len()).min(s.len());
    if from >= end || end - from < pat.len() {
        return None;
    }
    s[from..end].windows(pat.len()).position(|w| w == pat).map(|k| from + k)
}

fn inline(s: &[u8], m: &mut Markup, depth: u32) {
    let mut i = 0;
    let mut plain = 0; // start of the plain text not yet written
    while i < s.len() {
        let c = s[i];
        let mut done = None; // Some(next i) if something was written
        if c == b'\\' && i + 1 < s.len() && s[i + 1].is_ascii_punctuation() {
            m.text(&s[plain..i]);
            m.text(&s[i + 1..i + 2]);
            done = Some(i + 2);
        } else if c == b'`' {
            if let Some(e) = find(s, i + 1, b"`", WINDOW) {
                if e > i + 1 {
                    m.text(&s[plain..i]);
                    m.tag(Tag::Code);
                    m.text(&s[i + 1..e]);
                    m.tag(Tag::Code_);
                    done = Some(e + 1);
                }
            }
        } else if c == b'*' && s.get(i + 1) == Some(&b'*') && depth < INLINE_DEPTH {
            if let Some(e) = find(s, i + 2, b"**", WINDOW) {
                if e > i + 2 {
                    m.text(&s[plain..i]);
                    m.tag(Tag::Strong);
                    inline(&s[i + 2..e], m, depth + 1);
                    m.tag(Tag::Strong_);
                    done = Some(e + 2);
                }
            }
        } else if (c == b'*' || c == b'_') && depth < INLINE_DEPTH && i + 1 < s.len() && s[i + 1] != b' ' {
            if let Some(e) = find(s, i + 1, &[c], WINDOW) {
                if e > i + 1 && s[e - 1] != b' ' {
                    m.text(&s[plain..i]);
                    m.tag(Tag::Em);
                    inline(&s[i + 1..e], m, depth + 1);
                    m.tag(Tag::Em_);
                    done = Some(e + 1);
                }
            }
        } else if c == b'[' && depth < INLINE_DEPTH {
            if let Some(te) = find(s, i + 1, b"](", LINK_TEXT) {
                if let Some(ue) = find(s, te + 2, b")", URL_MAX) {
                    let url = &s[te + 2..ue];
                    if te > i + 1 && crate::html::url_ok_exec(url) {
                        m.text(&s[plain..i]);
                        m.link(url, None);
                        inline(&s[i + 1..te], m, depth + 1);
                        m.tag(Tag::A_);
                        done = Some(ue + 1);
                    }
                }
            }
        }
        match done {
            Some(n) => {
                i = n;
                plain = n;
            }
            None => i += 1,
        }
    }
    m.text(&s[plain..]);
}

/// Words in Markdown (for "N min read").
pub fn words(md: &[u8]) -> u64 {
    let mut n = 0;
    let mut in_word = false;
    for &b in md {
        let w = b.is_ascii_alphanumeric() || b >= 0x80;
        if w && !in_word {
            n += 1;
        }
        in_word = w;
    }
    n
}
