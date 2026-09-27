// CommonMark (0.31.2) to HTML, through html::Markup. Tested against every
// example of the spec (src/tests/markdown.rs, tests/commonmark/spec.json).
// Safety does not depend on this file: Markup can only hold allowed markup
// (proved, src/html.rs), so the deviations from the spec are exactly the
// law's: raw HTML is shown as text, and a link or image whose URL has a
// scheme other than http, https or mailto is shown as its text.
//
// The algorithm is the spec's (as in its reference implementations): a
// block pass line by line with a stack of open containers, then an inline
// pass per paragraph and heading (a delimiter stack for emphasis, a bracket
// stack for links). Nodes live in one arena, linked by index.
//
// Linear time: every search is bounded or advances the position, and the
// delimiter and bracket algorithms are the spec's (linear with the
// openers-bottom cache).
use crate::html::Markup;
use crate::md_tables::{ENTITIES, PUNCT, SPACE};
use crate::spec_markup::Tag;
use std::collections::HashMap;

const NONE: usize = usize::MAX;
const CODE_INDENT: usize = 4;
const MAX_NEST: usize = 64; // (containers and inline nesting: the renderer's recursion)

#[derive(Clone, Copy, PartialEq, Debug)]
enum K {
    Document,
    Quote,
    List,
    Item,
    Paragraph,
    Heading,
    Break,
    CodeBlock,
    Text,
    Soft,
    Hard,
    Emph,
    Strong,
    Code,
    Link,
    Image,
}

#[derive(Clone, Copy, PartialEq)]
struct ListData {
    ordered: bool,
    bullet: u8,
    start: u64,
    delim: u8,
    padding: usize,
    marker_offset: usize,
    tight: bool,
}

const NO_LIST: ListData = ListData { ordered: false, bullet: 0, start: 0, delim: 0, padding: 0, marker_offset: 0, tight: true };

struct Node {
    k: K,
    parent: usize,
    first: usize,
    last: usize,
    next: usize,
    prev: usize,
    open: bool,
    start_line: usize,
    end_line: usize,
    // The text (paragraphs, headings, code blocks; an inline's literal): a
    // range of the tree's one byte buffer.
    cs: usize,
    cl: usize,
    // What only some nodes have (lists, items, code blocks, headings,
    // links, images): an index into the tree's extras, or NONE. Keeps every
    // node small (the arena is walked and dropped whole).
    x: usize,
}

#[derive(Default)]
struct Extra {
    info: Vec<u8>,
    dest: Vec<u8>,
    title: Vec<u8>,
    level: u8,
    fenced: bool,
    fence_char: u8,
    fence_len: usize,
    fence_offset: usize,
    list: ListData,
}

impl Default for ListData {
    fn default() -> Self {
        NO_LIST
    }
}

struct Tree {
    n: Vec<Node>,
    buf: Vec<u8>,
    xs: Vec<Extra>,
}

static NO_EXTRA: Extra = Extra { info: Vec::new(), dest: Vec::new(), title: Vec::new(), level: 0, fenced: false, fence_char: 0, fence_len: 0, fence_offset: 0, list: NO_LIST };

impl Tree {
    fn add(&mut self, k: K, line: usize) -> usize {
        self.n.push(Node {
            k,
            parent: NONE,
            first: NONE,
            last: NONE,
            next: NONE,
            prev: NONE,
            open: true,
            start_line: line,
            end_line: line,
            cs: 0,
            cl: 0,
            x: NONE,
        });
        self.n.len() - 1
    }
    /// Node n's extras, made if it has none.
    fn ex(&mut self, n: usize) -> &mut Extra {
        if self.n[n].x == NONE {
            self.xs.push(Extra::default());
            self.n[n].x = self.xs.len() - 1;
        }
        &mut self.xs[self.n[n].x]
    }
    /// Node n's extras (the defaults if it has none).
    fn xr(&self, n: usize) -> &Extra {
        if self.n[n].x == NONE { &NO_EXTRA } else { &self.xs[self.n[n].x] }
    }
    fn text(&mut self, s: &[u8]) -> usize {
        let t = self.add(K::Text, 0);
        self.set(t, s);
        t
    }
    /// A node's text.
    fn bytes(&self, n: usize) -> &[u8] {
        &self.buf[self.n[n].cs..self.n[n].cs + self.n[n].cl]
    }
    /// Sets a node's text (a copy at the buffer's end).
    fn set(&mut self, n: usize, s: &[u8]) {
        self.n[n].cs = self.buf.len();
        self.n[n].cl = s.len();
        self.buf.extend_from_slice(s);
    }
    /// Makes n's text end at the buffer's end (so it can grow there).
    fn to_tail(&mut self, n: usize) {
        let (cs, cl) = (self.n[n].cs, self.n[n].cl);
        if cl == 0 {
            self.n[n].cs = self.buf.len();
        } else if cs + cl != self.buf.len() {
            let copy = self.buf[cs..cs + cl].to_vec();
            self.set(n, &copy);
        }
    }
    fn append(&mut self, parent: usize, c: usize) {
        self.unlink(c);
        let last = self.n[parent].last;
        self.n[c].parent = parent;
        self.n[c].prev = last;
        self.n[c].next = NONE;
        if last != NONE {
            self.n[last].next = c;
        } else {
            self.n[parent].first = c;
        }
        self.n[parent].last = c;
    }
    fn unlink(&mut self, c: usize) {
        let (p, prev, next) = (self.n[c].parent, self.n[c].prev, self.n[c].next);
        if prev != NONE {
            self.n[prev].next = next;
        } else if p != NONE {
            self.n[p].first = next;
        }
        if next != NONE {
            self.n[next].prev = prev;
        } else if p != NONE {
            self.n[p].last = prev;
        }
        self.n[c].parent = NONE;
        self.n[c].prev = NONE;
        self.n[c].next = NONE;
    }
    fn insert_after(&mut self, at: usize, c: usize) {
        self.unlink(c);
        let (p, next) = (self.n[at].parent, self.n[at].next);
        self.n[c].parent = p;
        self.n[c].prev = at;
        self.n[c].next = next;
        self.n[at].next = c;
        if next != NONE {
            self.n[next].prev = c;
        } else if p != NONE {
            self.n[p].last = c;
        }
    }
}

// CHARACTERS
fn in_ranges(c: u32, r: &[(u32, u32)]) -> bool {
    r.binary_search_by(|&(a, b)| if b < c { std::cmp::Ordering::Less } else if a > c { std::cmp::Ordering::Greater } else { std::cmp::Ordering::Equal }).is_ok()
}
fn is_punct(c: char) -> bool {
    c.is_ascii_punctuation() || (c as u32 >= 0x80 && in_ranges(c as u32, &PUNCT))
}
fn is_uspace(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\x0c' | '\r') || in_ranges(c as u32, &SPACE)
}
fn space_or_tab(b: Option<&u8>) -> bool {
    matches!(b, Some(b' ') | Some(b'\t'))
}
fn is_escapable(b: u8) -> bool {
    b.is_ascii_punctuation()
}

/// The character (code point) starting at byte i of s (s is UTF-8).
fn char_at(s: &[u8], i: usize) -> Option<char> {
    std::str::from_utf8(&s[i..(i + 4).min(s.len())]).map_or_else(|e| std::str::from_utf8(&s[i..i + e.valid_up_to()]).ok(), Some).and_then(|t| t.chars().next())
}
/// The character ending just before byte i.
fn char_before(s: &[u8], i: usize) -> Option<char> {
    if i == 0 {
        return None;
    }
    let mut j = i - 1;
    while j > 0 && (s[j] & 0xc0) == 0x80 && i - j < 4 {
        j -= 1;
    }
    char_at(s, j)
}

// ENTITIES AND ESCAPES
fn entity(name: &str) -> Option<&'static str> {
    ENTITIES.binary_search_by(|(k, _)| (*k).cmp(name)).ok().map(|i| ENTITIES[i].1)
}

/// An entity or numeric character reference at s[i] ('&'): its text and
/// length.
fn char_ref(s: &[u8], i: usize) -> Option<(String, usize)> {
    let rest = &s[i + 1..];
    if rest.first() == Some(&b'#') {
        let (hex, digits) = if matches!(rest.get(1), Some(b'x') | Some(b'X')) { (true, &rest[2..]) } else { (false, &rest[1..]) };
        let n = digits.iter().take_while(|c| if hex { c.is_ascii_hexdigit() } else { c.is_ascii_digit() }).count();
        if n == 0 || n > if hex { 6 } else { 7 } || digits.get(n) != Some(&b';') {
            return None;
        }
        let v = u32::from_str_radix(std::str::from_utf8(&digits[..n]).unwrap(), if hex { 16 } else { 10 }).unwrap();
        let c = if v == 0 { '\u{fffd}' } else { char::from_u32(v).unwrap_or('\u{fffd}') };
        return Some((c.to_string(), 1 + (if hex { 2 } else { 1 }) + n + 1));
    }
    let n = rest.iter().take(32).take_while(|c| c.is_ascii_alphanumeric()).count();
    if n == 0 || !rest[0].is_ascii_alphabetic() || rest.get(n) != Some(&b';') {
        return None;
    }
    let name = std::str::from_utf8(&rest[..n]).unwrap();
    entity(name).map(|t| (t.to_string(), n + 2))
}

/// Backslash escapes and character references resolved.
fn unescape(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'\\' && i + 1 < s.len() && is_escapable(s[i + 1]) {
            out.push(s[i + 1]);
            i += 2;
        } else if s[i] == b'&' {
            if let Some((t, n)) = char_ref(s, i) {
                out.extend_from_slice(t.as_bytes());
                i += n;
            } else {
                out.push(b'&');
                i += 1;
            }
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    out
}

/// A URL percent-encoded as the reference implementation does (mdurl's
/// encode): unreserved and reserved characters stay, a valid %XX stays,
/// everything else (spaces, non-ASCII, < > " \ ` [ ] ...) is encoded.
fn normalize_uri(s: &[u8]) -> Vec<u8> {
    let keep = |c: u8| c.is_ascii_alphanumeric() || b";/?:@&=+$,-_.!~*'()#".contains(&c);
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        let c = s[i];
        if c == b'%' && i + 2 < s.len() + 0 && i + 2 <= s.len() - 1 && s[i + 1].is_ascii_hexdigit() && s[i + 2].is_ascii_hexdigit() {
            out.extend_from_slice(&s[i..i + 3]);
            i += 3;
        } else if keep(c) {
            out.push(c);
            i += 1;
        } else {
            out.extend_from_slice(format!("%{:02X}", c).as_bytes());
            i += 1;
        }
    }
    out
}

/// A link label, normalized: inner whitespace collapsed, case folded.
fn normalize_label(s: &[u8]) -> String {
    let t = String::from_utf8_lossy(s);
    let collapsed: Vec<&str> = t.split(|c: char| matches!(c, ' ' | '\t' | '\r' | '\n')).filter(|w| !w.is_empty()).collect();
    collapsed.join(" ").to_lowercase().to_uppercase()
}

// BLOCKS
struct Blocks<'a> {
    t: Tree,
    line: Vec<u8>,
    line_no: usize,
    offset: usize,
    column: usize,
    next_nonspace: usize,
    next_nonspace_column: usize,
    indent: usize,
    indented: bool,
    blank: bool,
    partial_tab: bool,
    tip: usize,
    old_tip: usize,
    all_closed: bool,
    last_matched: usize,
    refs: HashMap<String, (Vec<u8>, Vec<u8>)>,
    _src: std::marker::PhantomData<&'a ()>,
}

fn can_contain(parent: K, child: K) -> bool {
    match parent {
        K::Document | K::Quote | K::Item => child != K::Item,
        K::List => child == K::Item,
        _ => false,
    }
}

fn accepts_lines(k: K) -> bool {
    k == K::Paragraph || k == K::CodeBlock
}

impl<'a> Blocks<'a> {
    fn peek(&self, i: usize) -> Option<&u8> {
        self.line.get(i)
    }

    fn find_next_nonspace(&mut self) {
        let mut i = self.offset;
        let mut cols = self.column;
        while i < self.line.len() {
            match self.line[i] {
                b' ' => {
                    i += 1;
                    cols += 1;
                }
                b'\t' => {
                    i += 1;
                    cols += 4 - cols % 4;
                }
                _ => break,
            }
        }
        self.blank = i == self.line.len();
        self.next_nonspace = i;
        self.next_nonspace_column = cols;
        self.indent = cols - self.column;
        self.indented = self.indent >= CODE_INDENT;
    }

    fn advance_next_nonspace(&mut self) {
        self.offset = self.next_nonspace;
        self.column = self.next_nonspace_column;
        self.partial_tab = false;
    }

    fn advance_offset(&mut self, mut count: usize, columns: bool) {
        while count > 0 && self.offset < self.line.len() {
            if self.line[self.offset] == b'\t' {
                let to_tab = 4 - self.column % 4;
                if columns {
                    self.partial_tab = to_tab > count;
                    let adv = to_tab.min(count);
                    self.column += adv;
                    if !self.partial_tab {
                        self.offset += 1;
                    }
                    count -= adv;
                } else {
                    self.partial_tab = false;
                    self.column += to_tab;
                    self.offset += 1;
                    count -= 1;
                }
            } else {
                self.partial_tab = false;
                self.offset += 1;
                self.column += 1;
                count -= 1;
            }
        }
    }

    fn add_line(&mut self) {
        let tip = self.tip;
        self.t.to_tail(tip);
        let before = self.t.buf.len();
        if self.partial_tab {
            self.offset += 1;
            let to_tab = 4 - self.column % 4;
            for _ in 0..to_tab {
                self.t.buf.push(b' ');
            }
        }
        let from = self.offset.min(self.line.len());
        self.t.buf.extend_from_slice(&self.line[from..]);
        self.t.buf.push(b'\n');
        self.t.n[tip].cl += self.t.buf.len() - before;
    }

    fn add_child(&mut self, k: K) -> usize {
        while !can_contain(self.t.n[self.tip].k, k) {
            let tip = self.tip;
            self.finalize(tip, self.line_no - 1);
        }
        let c = self.t.add(k, self.line_no);
        self.t.append(self.tip, c);
        self.tip = c;
        c
    }

    fn close_unmatched(&mut self) {
        if !self.all_closed {
            while self.old_tip != self.last_matched {
                let parent = self.t.n[self.old_tip].parent;
                let old = self.old_tip;
                self.finalize(old, self.line_no - 1);
                self.old_tip = parent;
            }
            self.all_closed = true;
        }
    }

    fn finalize(&mut self, b: usize, line: usize) {
        let above = self.t.n[b].parent;
        self.t.n[b].open = false;
        self.t.n[b].end_line = line;
        match self.t.n[b].k {
            K::Paragraph => {
                self.strip_references(b);
                let blank = self.t.bytes(b).iter().all(|&c| matches!(c, b' ' | b'\t' | b'\n' | b'\r'));
                if blank {
                    self.t.unlink(b);
                }
            }
            K::CodeBlock => {
                let content = self.t.bytes(b).to_vec();
                if self.t.xr(b).fenced {
                    let nl = content.iter().position(|&c| c == b'\n').unwrap_or(content.len());
                    let first = &content[..nl];
                    let info = trim_ws(first);
                    self.t.ex(b).info = unescape(info);
                    let skip = (nl + 1).min(content.len());
                    self.t.n[b].cs += skip;
                    self.t.n[b].cl -= skip;
                } else {
                    // Trailing blank lines are not part of it.
                    let mut end = content.len();
                    let mut k = content.len();
                    while k > 0 {
                        let ls = content[..k - 1].iter().rposition(|&c| c == b'\n').map_or(0, |p| p + 1);
                        if content[ls..k - 1].iter().all(|&c| c == b' ' || c == b'\t') {
                            end = ls;
                            k = ls;
                        } else {
                            break;
                        }
                    }
                    let mut c = content[..end].to_vec();
                    if c.last() != Some(&b'\n') && !c.is_empty() {
                        c.push(b'\n');
                    }
                    self.t.set(b, &c);
                }
            }
            K::List => {
                let mut tight = true;
                let mut item = self.t.n[b].first;
                'items: while item != NONE {
                    if self.t.n[item].next != NONE && self.ends_with_blank(item) {
                        tight = false;
                        break;
                    }
                    let mut sub = self.t.n[item].first;
                    while sub != NONE {
                        if self.t.n[sub].next != NONE && self.ends_with_blank(sub) {
                            tight = false;
                            break 'items;
                        }
                        sub = self.t.n[sub].next;
                    }
                    item = self.t.n[item].next;
                }
                self.t.ex(b).list.tight = tight;
                let last = self.t.n[b].last;
                if last != NONE {
                    self.t.n[b].end_line = self.t.n[last].end_line;
                }
            }
            K::Item => {
                let last = self.t.n[b].last;
                self.t.n[b].end_line = if last != NONE { self.t.n[last].end_line } else { self.t.n[b].start_line };
            }
            _ => {}
        }
        self.tip = above;
    }

    /// Link reference definitions at the start of paragraph b are not its
    /// text (they are recorded).
    fn strip_references(&mut self, b: usize) {
        // (The buffer is set aside while the definitions are read from it:
        // parse_reference only records them. No copy: a paragraph of many
        // definitions would be quadratic.)
        let buf = std::mem::take(&mut self.t.buf);
        loop {
            let (cs, cl) = (self.t.n[b].cs, self.t.n[b].cl);
            let content = &buf[cs..cs + cl];
            if content.first() != Some(&b'[') {
                break;
            }
            let n = self.parse_reference(content);
            if n == 0 {
                break;
            }
            self.t.n[b].cs += n;
            self.t.n[b].cl -= n;
        }
        self.t.buf = buf;
    }

    /// A blank line between this block and the next.
    fn ends_with_blank(&self, b: usize) -> bool {
        let nx = self.t.n[b].next;
        nx != NONE && self.t.n[b].end_line + 1 < self.t.n[nx].start_line
    }

    /// A link reference definition at the start of s: its length (0: none).
    fn parse_reference(&mut self, s: &[u8]) -> usize {
        let mut p = InlineCursor { s, pos: 0 };
        let n = p.link_label();
        if n == 0 {
            return 0;
        }
        let raw = &s[..n];
        p.pos = n;
        if p.peek() != Some(b':') {
            return 0;
        }
        p.pos += 1;
        p.spnl();
        let Some(dest) = p.link_destination() else { return 0 };
        let before_title = p.pos;
        p.spnl();
        let mut title = None;
        if p.pos != before_title {
            title = p.link_title();
        }
        if title.is_none() {
            p.pos = before_title;
        }
        // The rest of the line must be blank.
        let mut at_eol = true;
        let save = p.pos;
        p.skip_spaces_tabs();
        if !(p.peek().is_none() || p.peek() == Some(b'\n')) {
            if title.is_some() {
                // The title may not be one: try without it.
                title = None;
                p.pos = before_title;
                p.skip_spaces_tabs();
                if !(p.peek().is_none() || p.peek() == Some(b'\n')) {
                    at_eol = false;
                }
            } else {
                at_eol = false;
            }
        } else {
            let _ = save;
        }
        if !at_eol {
            return 0;
        }
        if p.peek() == Some(b'\n') {
            p.pos += 1;
        }
        let label = normalize_label(&raw[1..raw.len() - 1]);
        if label.is_empty() {
            return 0;
        }
        self.refs.entry(label).or_insert((dest, title.unwrap_or_default()));
        p.pos
    }

    // Continuing an open container on this line: 0 matched, 1 not, 2 the
    // line is used up (a closing fence).
    fn continues(&mut self, c: usize) -> u8 {
        match self.t.n[c].k {
            K::Document | K::List => 0,
            K::Quote => {
                if !self.indented && self.peek(self.next_nonspace) == Some(&b'>') {
                    self.advance_next_nonspace();
                    self.advance_offset(1, false);
                    if space_or_tab(self.peek(self.offset)) {
                        self.advance_offset(1, true);
                    }
                    0
                } else {
                    1
                }
            }
            K::Item => {
                if self.blank {
                    if self.t.n[c].first == NONE {
                        return 1;
                    }
                    self.advance_next_nonspace();
                    0
                } else {
                    let l = self.t.xr(c).list;
                    if self.indent >= l.marker_offset + l.padding {
                        self.advance_offset(l.marker_offset + l.padding, true);
                        0
                    } else {
                        1
                    }
                }
            }
            K::Heading | K::Break => 1,
            K::CodeBlock => {
                let n = self.t.xr(c);
                if n.fenced {
                    let (fc, fl, fo) = (n.fence_char, n.fence_len, n.fence_offset);
                    if self.indent <= 3 && self.peek(self.next_nonspace) == Some(&fc) {
                        let s = &self.line[self.next_nonspace..];
                        let run = s.iter().take_while(|&&b| b == fc).count();
                        if run >= fl && s[run..].iter().all(|&b| b == b' ' || b == b'\t') {
                            let ln = self.line_no;
                            self.finalize(c, ln);
                            return 2;
                        }
                    }
                    let mut i = fo;
                    while i > 0 && space_or_tab(self.peek(self.offset)) {
                        self.advance_offset(1, true);
                        i -= 1;
                    }
                    0
                } else if self.indent >= CODE_INDENT {
                    self.advance_offset(CODE_INDENT, true);
                    0
                } else if self.blank {
                    self.advance_next_nonspace();
                    0
                } else {
                    1
                }
            }
            K::Paragraph => {
                if self.blank {
                    1
                } else {
                    0
                }
            }
            _ => 1,
        }
    }

    fn parse_list_marker(&mut self, container: usize) -> Option<ListData> {
        if self.indent >= 4 {
            return None;
        }
        let rest = &self.line[self.next_nonspace..];
        let mut d = NO_LIST;
        let marker_len;
        if matches!(rest.first(), Some(b'*') | Some(b'+') | Some(b'-')) {
            d.bullet = rest[0];
            marker_len = 1;
        } else {
            let n = rest.iter().take(10).take_while(|c| c.is_ascii_digit()).count();
            if n >= 1 && n <= 9 && matches!(rest.get(n), Some(b'.') | Some(b')')) {
                let start: u64 = std::str::from_utf8(&rest[..n]).unwrap().parse().unwrap();
                if self.t.n[container].k == K::Paragraph && start != 1 {
                    return None;
                }
                d.ordered = true;
                d.start = start;
                d.delim = rest[n];
                marker_len = n + 1;
            } else {
                return None;
            }
        }
        let after = self.peek(self.next_nonspace + marker_len).copied();
        if !(after.is_none() || after == Some(b'\t') || after == Some(b' ')) {
            return None;
        }
        if self.t.n[container].k == K::Paragraph && self.line[self.next_nonspace + marker_len..].iter().all(|&c| c == b' ' || c == b'\t') {
            return None;
        }
        self.advance_next_nonspace();
        self.advance_offset(marker_len, true);
        let spaces_start_col = self.column;
        let spaces_start_offset = self.offset;
        loop {
            self.advance_offset(1, true);
            let nx = self.peek(self.offset);
            if !(self.column - spaces_start_col < 5 && space_or_tab(nx)) {
                break;
            }
        }
        let blank_item = self.peek(self.offset).is_none();
        let spaces_after = self.column - spaces_start_col;
        if spaces_after >= 5 || spaces_after < 1 || blank_item {
            d.padding = marker_len + 1;
            self.column = spaces_start_col;
            self.offset = spaces_start_offset;
            if space_or_tab(self.peek(self.offset)) {
                self.advance_offset(1, true);
            }
        } else {
            d.padding = marker_len + spaces_after;
        }
        d.marker_offset = self.indent;
        Some(d)
    }

    // Starting a block: 0 none, 1 a container, 2 a leaf.
    fn starts(&mut self, container: &mut usize) -> u8 {
        let c = *container;
        let nn = self.next_nonspace;
        let first = self.peek(nn).copied();
        // Block quote.
        if !self.indented && first == Some(b'>') {
            self.advance_next_nonspace();
            self.advance_offset(1, false);
            if space_or_tab(self.peek(self.offset)) {
                self.advance_offset(1, true);
            }
            self.close_unmatched();
            *container = self.add_child(K::Quote);
            return 1;
        }
        // ATX heading.
        if !self.indented && first == Some(b'#') {
            let s = &self.line[nn..];
            let level = s.iter().take_while(|&&b| b == b'#').count();
            if level <= 6 && (s.len() == level || s[level] == b' ' || s[level] == b'\t') {
                self.advance_next_nonspace();
                self.advance_offset(level, false);
                self.close_unmatched();
                let h = self.add_child(K::Heading);
                *container = h;
                self.t.ex(h).level = level as u8;
                let mut text = trim_ws(&self.line[self.offset..]).to_vec();
                // A closing sequence of #s (after a space, or all of it).
                let hashes = text.iter().rev().take_while(|&&b| b == b'#').count();
                if hashes == text.len() {
                    text.clear();
                } else if hashes > 0 && matches!(text[text.len() - hashes - 1], b' ' | b'\t') {
                    text.truncate(text.len() - hashes);
                    let t2 = trim_ws(&text).to_vec();
                    text = t2;
                }
                self.t.set(h, &text);
                let rest = self.line.len() - self.offset;
                self.advance_offset(rest, false);
                return 2;
            }
        }
        // Fenced code.
        if !self.indented && (first == Some(b'`') || first == Some(b'~')) {
            let fc = first.unwrap();
            let s = &self.line[nn..];
            let run = s.iter().take_while(|&&b| b == fc).count();
            if run >= 3 && !(fc == b'`' && s[run..].contains(&b'`')) {
                self.close_unmatched();
                let b = self.add_child(K::CodeBlock);
                *container = b;
                let indent = self.indent;
                let n = self.t.ex(b);
                n.fenced = true;
                n.fence_char = fc;
                n.fence_len = run;
                n.fence_offset = indent;
                self.advance_next_nonspace();
                self.advance_offset(run, false);
                return 2;
            }
        }
        // Setext heading (underlining a paragraph).
        if !self.indented && self.t.n[c].k == K::Paragraph && (first == Some(b'=') || first == Some(b'-')) {
            let s = &self.line[nn..];
            let ch = first.unwrap();
            let run = s.iter().take_while(|&&b| b == ch).count();
            if s[run..].iter().all(|&b| b == b' ' || b == b'\t') {
                self.close_unmatched();
                // Reference definitions at the paragraph's start are not text.
                self.strip_references(c);
                if self.t.n[c].cl > 0 {
                    let h = self.t.add(K::Heading, self.t.n[c].start_line);
                    self.t.ex(h).level = if ch == b'=' { 1 } else { 2 };
                    self.t.n[h].cs = self.t.n[c].cs;
                    self.t.n[h].cl = self.t.n[c].cl;
                    self.t.insert_after(c, h);
                    self.t.unlink(c);
                    self.tip = h;
                    *container = h;
                    let rest = self.line.len() - self.offset;
                    self.advance_offset(rest, false);
                    return 2;
                }
            }
        }
        // Thematic break.
        if !self.indented && matches!(first, Some(b'*') | Some(b'_') | Some(b'-')) {
            let ch = first.unwrap();
            let s = &self.line[nn..];
            let marks = s.iter().filter(|&&b| b == ch).count();
            if marks >= 3 && s.iter().all(|&b| b == ch || b == b' ' || b == b'\t') {
                self.close_unmatched();
                *container = self.add_child(K::Break);
                let rest = self.line.len() - self.offset;
                self.advance_offset(rest, false);
                return 2;
            }
        }
        // List item.
        if !self.indented || self.t.n[c].k == K::List {
            if let Some(data) = self.parse_list_marker(c) {
                self.close_unmatched();
                let tip = self.tip;
                let same = self.t.n[tip].k == K::List && {
                    let l = self.t.xr(tip).list;
                    l.ordered == data.ordered && l.delim == data.delim && l.bullet == data.bullet
                };
                if !same {
                    let l = self.add_child(K::List);
                    self.t.ex(l).list = data;
                }
                let it = self.add_child(K::Item);
                self.t.ex(it).list = data;
                *container = it;
                return 1;
            }
        }
        // Indented code.
        if self.indented && self.t.n[self.tip].k != K::Paragraph && !self.blank {
            self.advance_offset(CODE_INDENT, true);
            self.close_unmatched();
            *container = self.add_child(K::CodeBlock);
            return 2;
        }
        0
    }

    fn incorporate(&mut self, line: &[u8]) {
        self.line.clear();
        // NUL becomes U+FFFD.
        for &b in line {
            if b == 0 {
                self.line.extend_from_slice("\u{fffd}".as_bytes());
            } else {
                self.line.push(b);
            }
        }
        self.line_no += 1;
        self.offset = 0;
        self.column = 0;
        self.blank = false;
        self.partial_tab = false;
        self.old_tip = self.tip;
        let mut container = 0;
        let mut depth = 0;
        loop {
            let last = self.t.n[container].last;
            if last == NONE || !self.t.n[last].open {
                break;
            }
            container = last;
            depth += 1;
            self.find_next_nonspace();
            match self.continues(container) {
                0 => {}
                1 => {
                    container = self.t.n[container].parent;
                    break;
                }
                _ => return,
            }
        }
        self.all_closed = container == self.old_tip;
        self.last_matched = container;
        let mut matched_leaf = self.t.n[container].k != K::Paragraph && accepts_lines(self.t.n[container].k);
        while !matched_leaf && depth < MAX_NEST {
            self.find_next_nonspace();
            let f = self.peek(self.next_nonspace).copied();
            let maybe = matches!(f, Some(b'#' | b'`' | b'~' | b'*' | b'+' | b'_' | b'=' | b'<' | b'>' | b'-' | b'0'..=b'9'));
            if !self.indented && !maybe {
                self.advance_next_nonspace();
                break;
            }
            match self.starts(&mut container) {
                1 => depth += 1,
                2 => {
                    matched_leaf = true;
                }
                _ => {
                    self.advance_next_nonspace();
                    break;
                }
            }
        }
        // What is left of the line is text.
        if !self.all_closed && !self.blank && self.t.n[self.tip].k == K::Paragraph {
            self.add_line(); // lazy continuation
        } else {
            self.close_unmatched();
            let k = self.t.n[container].k;
            if accepts_lines(k) {
                self.add_line();
            } else if self.offset < self.line.len() && !self.blank {
                container = self.add_child(K::Paragraph);
                let _ = container;
                self.advance_next_nonspace();
                self.add_line();
            }
        }
    }
}

fn trim_ws(s: &[u8]) -> &[u8] {
    let a = s.iter().position(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')).unwrap_or(s.len());
    let b = s.iter().rposition(|&c| !matches!(c, b' ' | b'\t' | b'\n' | b'\r' | b'\x0c')).map_or(a, |p| p + 1);
    &s[a..b.max(a)]
}

// INLINES
struct InlineCursor<'s> {
    s: &'s [u8],
    pos: usize,
}

impl<'s> InlineCursor<'s> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }
    fn spnl(&mut self) {
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
        if self.peek() == Some(b'\n') {
            self.pos += 1;
            while self.peek() == Some(b' ') {
                self.pos += 1;
            }
        }
    }
    fn skip_spaces_tabs(&mut self) {
        while matches!(self.peek(), Some(b' ') | Some(b'\t')) {
            self.pos += 1;
        }
    }
    /// A link label [...] here: its length with the brackets (0: none).
    fn link_label(&self) -> usize {
        let s = self.s;
        let p = self.pos;
        if s.get(p) != Some(&b'[') {
            return 0;
        }
        let mut i = p + 1;
        let mut n = 0;
        while i < s.len() && n <= 999 {
            match s[i] {
                b'\\' if i + 1 < s.len() => {
                    i += 2;
                    n += 2;
                }
                b'[' => return 0,
                b']' => return i + 1 - p,
                _ => {
                    i += 1;
                    n += 1;
                }
            }
        }
        0
    }
    fn link_destination(&mut self) -> Option<Vec<u8>> {
        let s = self.s;
        if self.peek() == Some(b'<') {
            let mut i = self.pos + 1;
            while i < s.len() {
                match s[i] {
                    b'\\' if i + 1 < s.len() => i += 2,
                    b'>' => {
                        let d = normalize_uri(&unescape(&s[self.pos + 1..i]));
                        self.pos = i + 1;
                        return Some(d);
                    }
                    b'<' | b'\n' => return None,
                    _ => i += 1,
                }
            }
            return None;
        }
        let start = self.pos;
        let mut open = 0usize;
        while let Some(c) = self.peek() {
            if c == b'\\' && self.s.get(self.pos + 1).map_or(false, |&n| is_escapable(n)) {
                self.pos += 2;
            } else if c == b'(' {
                self.pos += 1;
                open += 1;
                if open > 32 {
                    return None;
                }
            } else if c == b')' {
                if open < 1 {
                    break;
                }
                self.pos += 1;
                open -= 1;
            } else if c <= 0x20 || c == 0x7f {
                break;
            } else {
                self.pos += 1;
            }
        }
        if self.pos == start && self.peek() != Some(b')') {
            return None;
        }
        if open != 0 {
            return None;
        }
        Some(normalize_uri(&unescape(&s[start..self.pos])))
    }
    fn link_title(&mut self) -> Option<Vec<u8>> {
        let s = self.s;
        let open = self.peek()?;
        let close = match open {
            b'"' => b'"',
            b'\'' => b'\'',
            b'(' => b')',
            _ => return None,
        };
        let mut i = self.pos + 1;
        while i < s.len() {
            if s[i] == b'\\' && i + 1 < s.len() {
                i += 2;
            } else if s[i] == close {
                let t = unescape(&s[self.pos + 1..i]);
                self.pos = i + 1;
                return Some(t);
            } else if open == b'(' && s[i] == b'(' {
                return None;
            } else {
                i += 1;
            }
        }
        None
    }
}

struct Delim {
    cc: u8,
    num: usize,
    orig: usize,
    node: usize,
    prev: usize,
    next: usize,
    can_open: bool,
    can_close: bool,
}

struct Bracket {
    node: usize,
    prev: usize,
    prev_delim: usize,
    index: usize,
    image: bool,
    active: bool,
    bracket_after: bool,
}

struct Inlines<'a, 'r> {
    t: &'a mut Tree,
    s: &'a [u8],
    pos: usize,
    delims: Vec<Delim>,
    top: usize, // top of the delimiter stack (index into delims)
    brackets: Vec<Bracket>,
    btop: usize,
    refs: &'r HashMap<String, (Vec<u8>, Vec<u8>)>,
}

impl<'a, 'r> Inlines<'a, 'r> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.pos).copied()
    }
    fn text(&mut self, block: usize, b: &[u8]) {
        let t = self.t.text(b);
        self.t.append(block, t);
    }
    fn parse(&mut self, block: usize) {
        while self.pos < self.s.len() {
            let c = self.s[self.pos];
            let done = match c {
                b'\n' => self.newline(block),
                b'\\' => self.backslash(block),
                b'`' => self.backticks(block),
                b'*' | b'_' => self.delim(block, c),
                b'[' => {
                    let n = self.t.text(b"[");
                    self.t.append(block, n);
                    self.add_bracket(n, self.pos, false);
                    self.pos += 1;
                    true
                }
                b'!' => {
                    if self.s.get(self.pos + 1) == Some(&b'[') {
                        let n = self.t.text(b"![");
                        self.t.append(block, n);
                        self.add_bracket(n, self.pos + 1, true);
                        self.pos += 2;
                    } else {
                        self.text(block, b"!");
                        self.pos += 1;
                    }
                    true
                }
                b']' => self.close_bracket(block),
                b'<' => self.autolink(block),
                b'&' => self.entity(block),
                _ => {
                    let start = self.pos;
                    while self.pos < self.s.len() && !matches!(self.s[self.pos], b'\n' | b'\\' | b'`' | b'*' | b'_' | b'[' | b'!' | b']' | b'<' | b'&') {
                        self.pos += 1;
                    }
                    let seg = self.s[start..self.pos].to_vec();
                    self.text(block, &seg);
                    true
                }
            };
            if !done {
                let seg = [c];
                self.text(block, &seg);
                self.pos += 1;
            }
        }
        self.process_emphasis(NONE);
    }

    fn newline(&mut self, block: usize) -> bool {
        self.pos += 1;
        let last = self.t.n[block].last;
        let mut hard = false;
        if last != NONE && self.t.n[last].k == K::Text && self.t.bytes(last).last() == Some(&b' ') {
            let sp = self.t.bytes(last).iter().rev().take_while(|&&b| b == b' ').count();
            hard = sp >= 2;
            self.t.n[last].cl -= sp;
        }
        let n = self.t.add(if hard { K::Hard } else { K::Soft }, 0);
        self.t.append(block, n);
        while self.peek() == Some(b' ') {
            self.pos += 1;
        }
        true
    }

    fn backslash(&mut self, block: usize) -> bool {
        self.pos += 1;
        match self.peek() {
            Some(b'\n') => {
                self.pos += 1;
                let n = self.t.add(K::Hard, 0);
                self.t.append(block, n);
                while self.peek() == Some(b' ') {
                    self.pos += 1;
                }
            }
            Some(c) if is_escapable(c) => {
                self.text(block, &[c]);
                self.pos += 1;
            }
            _ => self.text(block, b"\\"),
        }
        true
    }

    fn backticks(&mut self, block: usize) -> bool {
        let start = self.pos;
        let n = self.s[start..].iter().take_while(|&&b| b == b'`').count();
        self.pos += n;
        let after = self.pos;
        let mut i = after;
        while i < self.s.len() {
            if self.s[i] == b'`' {
                let m = self.s[i..].iter().take_while(|&&b| b == b'`').count();
                if m == n {
                    let mut c: Vec<u8> = self.s[after..i].iter().map(|&b| if b == b'\n' { b' ' } else { b }).collect();
                    if c.len() >= 2 && c[0] == b' ' && c[c.len() - 1] == b' ' && c.iter().any(|&b| b != b' ') {
                        c = c[1..c.len() - 1].to_vec();
                    }
                    let node = self.t.add(K::Code, 0);
                    self.t.set(node, &c);
                    self.t.append(block, node);
                    self.pos = i + m;
                    return true;
                }
                i += m;
            } else {
                i += 1;
            }
        }
        // No closing run: the backticks are text.
        let seg = self.s[start..after].to_vec();
        self.text(block, &seg);
        true
    }

    fn scan_delims(&self, cc: u8) -> (usize, bool, bool) {
        let start = self.pos;
        let n = self.s[start..].iter().take_while(|&&b| b == cc).count();
        let before = char_before(self.s, start).unwrap_or('\n');
        let after = if start + n < self.s.len() { char_at(self.s, start + n).unwrap_or('\n') } else { '\n' };
        let aw = is_uspace(after);
        let ap = is_punct(after);
        let bw = is_uspace(before);
        let bp = is_punct(before);
        let left = !aw && (!ap || bw || bp);
        let right = !bw && (!bp || aw || ap);
        let (open, close) = if cc == b'_' { (left && (!right || bp), right && (!left || ap)) } else { (left, right) };
        (n, open, close)
    }

    fn delim(&mut self, block: usize, cc: u8) -> bool {
        let (n, can_open, can_close) = self.scan_delims(cc);
        let start = self.pos;
        self.pos += n;
        let seg = self.s[start..self.pos].to_vec();
        let node = self.t.text(&seg);
        self.t.append(block, node);
        self.delims.push(Delim { cc, num: n, orig: n, node, prev: self.top, next: NONE, can_open, can_close });
        let d = self.delims.len() - 1;
        if self.top != NONE {
            self.delims[self.top].next = d;
        }
        self.top = d;
        true
    }

    fn remove_delim(&mut self, d: usize) {
        let (p, n) = (self.delims[d].prev, self.delims[d].next);
        if p != NONE {
            self.delims[p].next = n;
        }
        if n == NONE {
            self.top = p;
        } else {
            self.delims[n].prev = p;
        }
    }

    fn process_emphasis(&mut self, bottom: usize) {
        let mut openers_bottom = [bottom; 14];
        let mut closer = self.top;
        while closer != NONE && self.delims[closer].prev != bottom {
            closer = self.delims[closer].prev;
        }
        while closer != NONE {
            let cc = self.delims[closer].cc;
            if !self.delims[closer].can_close {
                closer = self.delims[closer].next;
                continue;
            }
            let ob = (if cc == b'_' { 2 } else { 8 }) + (if self.delims[closer].can_open { 3 } else { 0 }) + self.delims[closer].orig % 3;
            let mut opener = self.delims[closer].prev;
            let mut found = false;
            while opener != NONE && opener != bottom && opener != openers_bottom[ob] {
                let (o, c) = (&self.delims[opener], &self.delims[closer]);
                let odd = (c.can_open || o.can_close) && c.orig % 3 != 0 && (o.orig + c.orig) % 3 == 0;
                if o.cc == c.cc && o.can_open && !odd {
                    found = true;
                    break;
                }
                opener = self.delims[opener].prev;
            }
            let old_closer = closer;
            if !found {
                closer = self.delims[closer].next;
            } else {
                let use_delims = if self.delims[closer].num >= 2 && self.delims[opener].num >= 2 { 2 } else { 1 };
                let (oi, ci) = (self.delims[opener].node, self.delims[closer].node);
                self.delims[opener].num -= use_delims;
                self.delims[closer].num -= use_delims;
                self.t.n[oi].cl -= use_delims;
                self.t.n[ci].cl -= use_delims;
                let emph = self.t.add(if use_delims == 1 { K::Emph } else { K::Strong }, 0);
                let mut tmp = self.t.n[oi].next;
                while tmp != NONE && tmp != ci {
                    let nx = self.t.n[tmp].next;
                    self.t.append(emph, tmp);
                    tmp = nx;
                }
                self.t.insert_after(oi, emph);
                // Delimiters between opener and closer go.
                let mut d = self.delims[closer].prev;
                while d != NONE && d != opener {
                    let p = self.delims[d].prev;
                    self.remove_delim(d);
                    d = p;
                }
                if self.delims[opener].num == 0 {
                    self.t.unlink(oi);
                    self.remove_delim(opener);
                }
                if self.delims[closer].num == 0 {
                    self.t.unlink(ci);
                    let nx = self.delims[closer].next;
                    self.remove_delim(closer);
                    closer = nx;
                }
            }
            if !found {
                openers_bottom[ob] = self.delims[old_closer].prev;
                if !self.delims[old_closer].can_open {
                    self.remove_delim(old_closer);
                }
            }
        }
        while self.top != NONE && self.top != bottom {
            let t = self.top;
            self.remove_delim(t);
        }
    }

    fn add_bracket(&mut self, node: usize, index: usize, image: bool) {
        if self.btop != NONE {
            self.brackets[self.btop].bracket_after = true;
        }
        self.brackets.push(Bracket { node, prev: self.btop, prev_delim: self.top, index, image, active: true, bracket_after: false });
        self.btop = self.brackets.len() - 1;
    }

    fn close_bracket(&mut self, block: usize) -> bool {
        let start = self.pos;
        self.pos += 1;
        let ob = self.btop;
        if ob == NONE {
            self.text(block, b"]");
            return true;
        }
        if !self.brackets[ob].active {
            self.text(block, b"]");
            self.btop = self.brackets[ob].prev;
            return true;
        }
        let image = self.brackets[ob].image;
        let save = self.pos;
        let mut dest = None;
        let mut title = Vec::new();
        let mut matched = false;
        if self.peek() == Some(b'(') {
            let mut c = InlineCursor { s: self.s, pos: self.pos + 1 };
            c.spnl();
            if let Some(d) = c.link_destination() {
                c.spnl();
                let mut ok = true;
                if c.pos > 0 && matches!(self.s[c.pos - 1], b' ' | b'\t' | b'\n') {
                    if let Some(t) = c.link_title() {
                        title = t;
                    }
                }
                c.spnl();
                if c.peek() != Some(b')') {
                    ok = false;
                }
                if ok {
                    self.pos = c.pos + 1;
                    dest = Some(d);
                    matched = true;
                }
            }
            if !matched {
                self.pos = save;
                title.clear();
            }
        }
        if !matched {
            let before_label = self.pos;
            let c = InlineCursor { s: self.s, pos: self.pos };
            let n = c.link_label();
            let mut label: Option<Vec<u8>> = None;
            if n > 2 {
                label = Some(self.s[before_label..before_label + n].to_vec());
                self.pos = before_label + n;
            } else if !self.brackets[ob].bracket_after {
                let mut l = vec![b'['];
                l.extend_from_slice(&self.s[self.brackets[ob].index..start]);
                l.push(b']');
                // (index points at "[" itself)
                label = Some(self.s[self.brackets[ob].index..start + 1].to_vec());
                let _ = l;
            }
            if n == 0 {
                self.pos = save;
            } else if n <= 2 {
                self.pos = before_label + n;
            }
            if let Some(l) = label {
                let key = normalize_label(&l[1..l.len() - 1]);
                if let Some((d, t)) = self.refs.get(&key) {
                    dest = Some(d.clone());
                    title = t.clone();
                    matched = true;
                } else if n > 2 {
                    self.pos = save;
                }
            }
        }
        if matched {
            let node = self.t.add(if image { K::Image } else { K::Link }, 0);
            self.t.ex(node).dest = dest.unwrap_or_default();
            self.t.ex(node).title = title;
            let opener_node = self.brackets[ob].node;
            let mut tmp = self.t.n[opener_node].next;
            while tmp != NONE {
                let nx = self.t.n[tmp].next;
                self.t.append(node, tmp);
                tmp = nx;
            }
            self.t.append(block, node);
            let pd = self.brackets[ob].prev_delim;
            self.process_emphasis(pd);
            self.btop = self.brackets[ob].prev;
            self.t.unlink(opener_node);
            if !image {
                let mut o = self.btop;
                while o != NONE {
                    if !self.brackets[o].image {
                        self.brackets[o].active = false;
                    }
                    o = self.brackets[o].prev;
                }
            }
            true
        } else {
            self.btop = self.brackets[ob].prev;
            self.pos = start + 1;
            self.text(block, b"]");
            true
        }
    }

    fn autolink(&mut self, block: usize) -> bool {
        let s = &self.s[self.pos..];
        let end = match s.iter().take(2048).position(|&b| b == b'>') {
            Some(e) => e,
            None => return false,
        };
        let inner = &s[1..end];
        // URI: scheme (2-32 chars) ':' then no < > or controls or spaces.
        let sl = inner.iter().take_while(|&&b| b.is_ascii_alphanumeric() || b == b'.' || b == b'+' || b == b'-').count();
        let is_uri = sl >= 2 && sl <= 32 && inner[0].is_ascii_alphabetic() && inner.get(sl) == Some(&b':') && inner[sl + 1..].iter().all(|&b| b > 0x20 && b != b'<' && b != b'>');
        let is_email = !is_uri && email_ok(inner);
        if !is_uri && !is_email {
            return false;
        }
        let dest = if is_email {
            let mut d = b"mailto:".to_vec();
            d.extend_from_slice(inner);
            normalize_uri(&d)
        } else {
            normalize_uri(inner)
        };
        let node = self.t.add(K::Link, 0);
        self.t.ex(node).dest = dest;
        let t = self.t.text(inner);
        self.t.append(node, t);
        self.t.append(block, node);
        self.pos += end + 1;
        true
    }

    fn entity(&mut self, block: usize) -> bool {
        match char_ref(self.s, self.pos) {
            Some((t, n)) => {
                self.text(block, t.as_bytes());
                self.pos += n;
                true
            }
            None => false,
        }
    }
}

fn email_ok(s: &[u8]) -> bool {
    let Some(at) = s.iter().position(|&b| b == b'@') else { return false };
    let local = &s[..at];
    let domain = &s[at + 1..];
    if local.is_empty() || !local.iter().all(|&b| b.is_ascii_alphanumeric() || b".!#$%&'*+/=?^_`{|}~-".contains(&b)) {
        return false;
    }
    if domain.is_empty() {
        return false;
    }
    domain.split(|&b| b == b'.').all(|l| {
        !l.is_empty() && l.len() <= 63 && l[0].is_ascii_alphanumeric() && l[l.len() - 1].is_ascii_alphanumeric() && l.iter().all(|&b| b.is_ascii_alphanumeric() || b == b'-')
    })
}

// RENDERING (the reference implementation's HTML, through Markup)
struct Out {
    m: Markup,
    last: u8,
}

impl Out {
    fn tag(&mut self, t: Tag) {
        self.m.tag(t);
        self.last = b'>';
    }
    fn text(&mut self, s: &[u8]) {
        if !s.is_empty() {
            self.m.text(s);
            self.last = s[s.len() - 1];
        }
    }
    fn cr(&mut self) {
        if self.last != b'\n' && self.last != 0 {
            self.m.text(b"\n");
            self.last = b'\n';
        }
    }
}

fn heading_tags(level: u8) -> (Tag, Tag) {
    match level {
        1 => (Tag::H1, Tag::H1_),
        2 => (Tag::H2, Tag::H2_),
        3 => (Tag::H3, Tag::H3_),
        4 => (Tag::H4, Tag::H4_),
        5 => (Tag::H5, Tag::H5_),
        _ => (Tag::H6, Tag::H6_),
    }
}

/// The plain text of inlines (an image's alt).
fn plain(t: &Tree, n: usize, out: &mut Vec<u8>, depth: usize) {
    let mut c = t.n[n].first;
    while c != NONE {
        match t.n[c].k {
            K::Text | K::Code => out.extend_from_slice(t.bytes(c)),
            K::Soft | K::Hard => out.push(b'\n'),
            _ if depth < MAX_NEST => plain(t, c, out, depth + 1),
            _ => {}
        }
        c = t.n[c].next;
    }
}

fn render_node(t: &Tree, n: usize, o: &mut Out, depth: usize) {
    if depth > MAX_NEST {
        return;
    }
    let node = &t.n[n];
    let kids = |o: &mut Out| {
        let mut c = t.n[n].first;
        while c != NONE {
            render_node(t, c, o, depth + 1);
            c = t.n[c].next;
        }
    };
    match node.k {
        K::Document => kids(o),
        K::Paragraph => {
            let gp = if node.parent != NONE { t.n[node.parent].parent } else { NONE };
            let tight = gp != NONE && t.n[gp].k == K::List && t.xr(gp).list.tight;
            if tight {
                kids(o);
            } else {
                o.cr();
                o.tag(Tag::P);
                kids(o);
                o.tag(Tag::P_);
                o.cr();
            }
        }
        K::Heading => {
            let (a, b) = heading_tags(t.xr(n).level);
            o.cr();
            o.tag(a);
            kids(o);
            o.tag(b);
            o.cr();
        }
        K::CodeBlock => {
            o.cr();
            o.tag(Tag::Pre);
            let lang: Vec<u8> = t.xr(n).info.iter().copied().take_while(|&b| !matches!(b, b' ' | b'\t' | b'\n')).collect();
            if lang.is_empty() {
                o.tag(Tag::Code);
            } else {
                o.m.code_lang(&lang);
                o.last = b'>';
            }
            o.text(t.bytes(n));
            o.tag(Tag::Code_);
            o.tag(Tag::Pre_);
            o.cr();
        }
        K::Break => {
            o.cr();
            o.tag(Tag::Hr);
            o.cr();
        }
        K::Quote => {
            o.cr();
            o.tag(Tag::Quote);
            o.cr();
            kids(o);
            o.cr();
            o.tag(Tag::Quote_);
            o.cr();
        }
        K::List => {
            o.cr();
            if t.xr(n).list.ordered {
                if t.xr(n).list.start != 1 {
                    o.m.ol_start(t.xr(n).list.start);
                    o.last = b'>';
                } else {
                    o.tag(Tag::Ol);
                }
            } else {
                o.tag(Tag::Ul);
            }
            o.cr();
            kids(o);
            o.cr();
            o.tag(if t.xr(n).list.ordered { Tag::Ol_ } else { Tag::Ul_ });
            o.cr();
        }
        K::Item => {
            o.tag(Tag::Li);
            kids(o);
            o.tag(Tag::Li_);
            o.cr();
        }
        K::Text => o.text(t.bytes(n)),
        K::Soft => o.text(b"\n"),
        K::Hard => {
            o.tag(Tag::Br);
            o.cr();
        }
        K::Emph => {
            o.tag(Tag::Em);
            kids(o);
            o.tag(Tag::Em_);
        }
        K::Strong => {
            o.tag(Tag::Strong);
            kids(o);
            o.tag(Tag::Strong_);
        }
        K::Code => {
            o.tag(Tag::Code);
            o.text(t.bytes(n));
            o.tag(Tag::Code_);
        }
        K::Link => {
            let title = if t.xr(n).title.is_empty() { None } else { Some(&t.xr(n).title[..]) };
            if o.m.link(&t.xr(n).dest, title) {
                o.last = b'>';
                kids(o);
                o.tag(Tag::A_);
            } else {
                kids(o); // (a URL the law does not allow: its text only)
            }
        }
        K::Image => {
            let mut alt = vec![];
            plain(t, n, &mut alt, depth);
            let title = if t.xr(n).title.is_empty() { None } else { Some(&t.xr(n).title[..]) };
            if o.m.img(&t.xr(n).dest, &alt, title) {
                o.last = b'>';
            } else {
                o.text(&alt);
            }
        }
    }
}

/// CommonMark to HTML (allowed markup only: html::Markup).
pub fn render(md: &[u8]) -> Markup {
    let mut b = Blocks {
        t: Tree { n: Vec::with_capacity(64), buf: Vec::with_capacity(md.len() + 64), xs: Vec::new() },
        line: Vec::new(),
        line_no: 0,
        offset: 0,
        column: 0,
        next_nonspace: 0,
        next_nonspace_column: 0,
        indent: 0,
        indented: false,
        blank: false,
        partial_tab: false,
        tip: 0,
        old_tip: 0,
        all_closed: true,
        last_matched: 0,
        refs: HashMap::new(),
        _src: std::marker::PhantomData,
    };
    let doc = b.t.add(K::Document, 1);
    b.tip = doc;
    b.old_tip = doc;
    b.last_matched = doc;
    // Lines: split at \n, \r\n or \r; a final line break ends no line.
    let mut lines: Vec<&[u8]> = vec![];
    let mut i = 0;
    let mut start = 0;
    while i < md.len() {
        if md[i] == b'\n' || md[i] == b'\r' {
            lines.push(&md[start..i]);
            if md[i] == b'\r' && md.get(i + 1) == Some(&b'\n') {
                i += 1;
            }
            start = i + 1;
        }
        i += 1;
    }
    if start < md.len() {
        lines.push(&md[start..]);
    }
    for l in &lines {
        b.incorporate(l);
    }
    let n_lines = lines.len();
    while b.tip != NONE {
        let tip = b.tip;
        b.finalize(tip, n_lines);
        if tip == doc {
            break;
        }
    }
    // Inlines, for paragraphs and headings.
    let refs = std::mem::take(&mut b.refs);
    let mut t = b.t;
    let mut stack = vec![doc];
    let mut subject: Vec<u8> = Vec::new();
    let (mut delims, mut brackets) = (Vec::new(), Vec::new());
    while let Some(n) = stack.pop() {
        let k = t.n[n].k;
        if k == K::Paragraph || k == K::Heading {
            subject.clear();
            subject.extend_from_slice(trim_ws(t.bytes(n)));
            delims.clear();
            brackets.clear();
            let mut p = Inlines { t: &mut t, s: &subject, pos: 0, delims: std::mem::take(&mut delims), top: NONE, brackets: std::mem::take(&mut brackets), btop: NONE, refs: &refs };
            p.parse(n);
            delims = std::mem::take(&mut p.delims);
            brackets = std::mem::take(&mut p.brackets);
        } else {
            let mut c = t.n[n].first;
            while c != NONE {
                stack.push(c);
                c = t.n[c].next;
            }
        }
    }
    let mut o = Out { m: Markup::new(), last: 0 };
    render_node(&t, doc, &mut o, 0);
    o.m
}

/// Words in a text (for "N min read").
pub fn words(md: &[u8]) -> u64 {
    let s = String::from_utf8_lossy(md);
    let mut n = 0;
    let mut in_word = false;
    for c in s.chars() {
        let w = c.is_alphanumeric();
        if w && !in_word {
            n += 1;
        }
        in_word = w;
    }
    n
}
