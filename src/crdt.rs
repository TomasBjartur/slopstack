// The collaborative text: a Fugue list CRDT (Weidner & Kleppmann, "The Art
// of the Fugue", 2023) over runs. The same code runs in the server and, as
// WebAssembly, in the browser (src/sys/wasm.rs), so both sides hold the
// same document by construction.
//
// THE MODEL (proved in Lean about a model, lean/Fugue.lean): the state is a
// set of elements, each a character with a unique id (rep, ctr), a parent
// (another element, or the root) and a side (left or right child); plus
// the set of deleted ids. The text is the in-order walk of that tree:
// left children (ascending id), the element, right children (ascending
// id). Merging two states is set union, so replicas that received the same
// operations hold the same state and show the same text, whatever the
// order they came in.
//
// THIS CODE (tested against the model: src/tests/crdt.rs replays random
// multi-replica histories here and in a naive tree walk, and requires the
// same text; tests/crdt_lean.sh does the same against the Lean model's own
// executable definition):
// - A run is a stretch of elements where each is the right child of the one
//   before (what typing or a paste makes): one record, one operation. Runs
//   are split when an element inside one gets a child or is deleted.
// - Records are columns (struct of arrays), indexed by u32.
// - Document order (tombstones included) is kept as blocks of run indexes;
//   a block knows its live length in UTF-16 units (the browser's unit), so
//   a position is found by summing blocks, then runs.
// - Ids map to runs through an ordered map of run starts; a character's
//   children through a hash map keyed by its id.
// No recursion: a tree can be millions deep.
use std::collections::{BTreeMap, HashMap};
use std::hash::{BuildHasherDefault, Hasher};

/// Run indexes per block (a block is split past this).
const BLOCK_MAX: usize = 64;
/// The root's key (rep 0 is never an element).
pub const ROOT: u64 = 0;
/// Replica 1 is the server's own (saves from the form without JavaScript);
/// editors get 2 and up, one per page load (src/docs.rs).
pub const SERVER_REP: u32 = 1;

pub const LEFT: u8 = 0;
pub const RIGHT: u8 = 1;

#[inline]
pub fn key(rep: u32, ctr: u32) -> u64 {
    (rep as u64) << 32 | ctr as u64
}

/// Why an operation was refused (the batch it came in is refused whole).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Bad {
    /// Malformed bytes.
    Format,
    /// An id already used, or rep or ctr 0, or a counter past 2^32.
    Id,
    /// A parent, or an element to delete, that this replica does not have.
    Missing,
    /// A position that is not in the text, or inside a character.
    Position,
}

/// One operation, as decoded (text borrowed from the batch).
#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Op<'a> {
    /// A run: elements (rep, ctr), (rep, ctr + 1), ... for the characters
    /// of text; the first is a child of parent on side, each next one the
    /// right child of the one before.
    Ins { rep: u32, ctr: u32, parent: u64, side: u8, text: &'a str },
    /// Deletes elements (rep, ctr) .. (rep, ctr + len).
    Del { rep: u32, ctr: u32, len: u32 },
}

// WIRE FORMAT (little-endian), operations one after another:
//   insert: 1, rep u32, ctr u32, parent rep u32, parent ctr u32, side u8,
//           byte length u32, UTF-8 text (at least one character)
//   delete: 2, rep u32, ctr u32, len u32 (at least 1)
pub fn encode(op: &Op, out: &mut Vec<u8>) {
    match *op {
        Op::Ins { rep, ctr, parent, side, text } => {
            out.push(1);
            out.extend_from_slice(&rep.to_le_bytes());
            out.extend_from_slice(&ctr.to_le_bytes());
            out.extend_from_slice(&((parent >> 32) as u32).to_le_bytes());
            out.extend_from_slice(&(parent as u32).to_le_bytes());
            out.push(side);
            out.extend_from_slice(&(text.len() as u32).to_le_bytes());
            out.extend_from_slice(text.as_bytes());
        }
        Op::Del { rep, ctr, len } => {
            out.push(2);
            out.extend_from_slice(&rep.to_le_bytes());
            out.extend_from_slice(&ctr.to_le_bytes());
            out.extend_from_slice(&len.to_le_bytes());
        }
    }
}

fn u32_at(b: &[u8], i: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(i..i + 4)?.try_into().ok()?))
}

/// The next operation in b from i, and where the one after starts.
pub fn decode(b: &[u8], i: usize) -> Result<(Op<'_>, usize), Bad> {
    let f = Bad::Format;
    match b.get(i) {
        Some(1) => {
            let rep = u32_at(b, i + 1).ok_or(f)?;
            let ctr = u32_at(b, i + 5).ok_or(f)?;
            let prep = u32_at(b, i + 9).ok_or(f)?;
            let pctr = u32_at(b, i + 13).ok_or(f)?;
            let side = *b.get(i + 17).ok_or(f)?;
            let n = u32_at(b, i + 18).ok_or(f)? as usize;
            let s = i + 22;
            let bytes = b.get(s..s.checked_add(n).ok_or(f)?).ok_or(f)?;
            let text = std::str::from_utf8(bytes).map_err(|_| f)?;
            if side > 1 || text.is_empty() {
                return Err(f);
            }
            Ok((Op::Ins { rep, ctr, parent: key(prep, pctr), side, text }, s + n))
        }
        Some(2) => {
            let rep = u32_at(b, i + 1).ok_or(f)?;
            let ctr = u32_at(b, i + 5).ok_or(f)?;
            let len = u32_at(b, i + 9).ok_or(f)?;
            if len == 0 {
                return Err(f);
            }
            Ok((Op::Del { rep, ctr, len }, i + 13))
        }
        _ => Err(f),
    }
}

/// A hasher for u64 keys (ids): one multiply. Keys come from other
/// replicas, so an attacker could aim for collisions: the map degrades to
/// slow, never wrong, and a post's operations are bounded (src/docs.rs).
#[derive(Default)]
pub struct IdHash(u64);

impl Hasher for IdHash {
    fn finish(&self) -> u64 {
        self.0
    }
    fn write(&mut self, _: &[u8]) {
        unreachable!("ids only")
    }
    fn write_u64(&mut self, k: u64) {
        self.0 = (k ^ (k >> 29)).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    }
}

type Map<V> = HashMap<u64, V, BuildHasherDefault<IdHash>>;

/// A character's children, as run indexes, ascending by id.
#[derive(Default, Clone)]
struct Kids {
    left: Vec<u32>,
    right: Vec<u32>,
}

struct Block {
    runs: Vec<u32>,
    /// Live length (UTF-16 units) of its runs.
    live: u64,
    /// Position in document order (of blocks).
    pos: u32,
}

/// Where a new run goes: before a run, after one, or first.
enum Place {
    Before(u32),
    After(u32),
    First,
}

/// Something the text changed by, for the view: at UTF-16 position pos,
/// del units removed, then ins put in.
pub trait Sink {
    fn change(&mut self, pos: u64, del: u64, ins: &str);
}

impl Sink for () {
    fn change(&mut self, _: u64, _: u64, _: &str) {}
}

pub struct Doc {
    // RUNS (columns). A run: elements (rep, ctr .. ctr + len); the first a
    // child of (prep, pctr) on side; text[off .. off + bytes], u16 units.
    rep: Vec<u32>,
    ctr: Vec<u32>,
    len: Vec<u32>,
    prep: Vec<u32>,
    pctr: Vec<u32>,
    side: Vec<u8>,
    dead: Vec<bool>,
    off: Vec<u32>,
    bytes: Vec<u32>,
    units: Vec<u32>,
    blk: Vec<u32>,
    /// Every character ever inserted (deleted ones stay: tombstones).
    text: Vec<u8>,
    /// Run starts by key.
    index: BTreeMap<u64, u32>,
    kids: Map<Kids>,
    /// Blocks by id, and their ids in document order.
    blocks: Vec<Block>,
    order: Vec<u32>,
    /// Live length in UTF-16 units, and in characters.
    live: u64,
    chars: u64,
}

impl Default for Doc {
    fn default() -> Self {
        Doc::new()
    }
}

fn units_of(s: &[u8]) -> u32 {
    // UTF-16 units of UTF-8 text: one per character, two for a 4-byte one.
    let mut n = 0u32;
    for &c in s {
        if c & 0xC0 != 0x80 {
            n += 1;
        }
        if c >= 0xF0 {
            n += 1;
        }
    }
    n
}

impl Doc {
    pub fn new() -> Doc {
        Doc {
            rep: vec![],
            ctr: vec![],
            len: vec![],
            prep: vec![],
            pctr: vec![],
            side: vec![],
            dead: vec![],
            off: vec![],
            bytes: vec![],
            units: vec![],
            blk: vec![],
            text: vec![],
            index: BTreeMap::new(),
            kids: Map::default(),
            blocks: vec![Block { runs: vec![], live: 0, pos: 0 }],
            order: vec![0],
            live: 0,
            chars: 0,
        }
    }

    /// Live length in UTF-16 units.
    pub fn len16(&self) -> u64 {
        self.live
    }

    /// Live characters.
    pub fn chars(&self) -> u64 {
        self.chars
    }

    /// Runs (for tests and limits).
    pub fn runs(&self) -> usize {
        self.rep.len()
    }

    /// Bytes this document holds in memory (roughly).
    pub fn memory(&self) -> usize {
        self.text.capacity() + self.rep.len() * (4 * 10 + 1 + 1) + self.index.len() * 24 + self.kids.len() * 64
    }

    pub fn text(&self) -> String {
        let mut s = Vec::with_capacity(self.chars as usize);
        for &b in &self.order {
            for &r in &self.blocks[b as usize].runs {
                if !self.dead[r as usize] {
                    let o = self.off[r as usize] as usize;
                    s.extend_from_slice(&self.text[o..o + self.bytes[r as usize] as usize]);
                }
            }
        }
        String::from_utf8(s).expect("runs hold whole characters")
    }

    // IDS
    fn first(&self, r: u32) -> u64 {
        key(self.rep[r as usize], self.ctr[r as usize])
    }

    fn last(&self, r: u32) -> u64 {
        key(self.rep[r as usize], self.ctr[r as usize] + self.len[r as usize] - 1)
    }

    /// The run holding element k, and k's place in it.
    fn find(&self, k: u64) -> Option<(u32, u32)> {
        let (&s, &r) = self.index.range(..=k).next_back()?;
        if s >> 32 != k >> 32 {
            return None;
        }
        let at = (k - s) as u32;
        if at < self.len[r as usize] { Some((r, at)) } else { None }
    }

    /// The next free counter of replica rep.
    pub fn next_ctr(&self, rep: u32) -> u32 {
        match self.index.range(key(rep, 0)..key(rep + 1, 0)).next_back() {
            Some((&s, &r)) => (s as u32) + self.len[r as usize],
            None => 1,
        }
    }

    /// Byte offset of character i in run r's text.
    fn byte_of(&self, r: u32, i: u32) -> u32 {
        let (o, n) = (self.off[r as usize] as usize, self.bytes[r as usize] as usize);
        if n == self.len[r as usize] as usize {
            return i;
        }
        let t = &self.text[o..o + n];
        let mut seen = 0;
        for (b, &c) in t.iter().enumerate() {
            if c & 0xC0 != 0x80 {
                if seen == i {
                    return b as u32;
                }
                seen += 1;
            }
        }
        n as u32
    }

    // BLOCKS
    fn place_in_block(&self, r: u32) -> usize {
        let b = &self.blocks[self.blk[r as usize] as usize];
        b.runs.iter().position(|&x| x == r).expect("a run is in its block")
    }

    fn live_units(&self, r: u32) -> u64 {
        if self.dead[r as usize] { 0 } else { self.units[r as usize] as u64 }
    }

    /// Puts run r into block b at index i (its live length counted).
    fn put(&mut self, b: u32, i: usize, r: u32) {
        self.blk[r as usize] = b;
        let lu = self.live_units(r);
        let blk = &mut self.blocks[b as usize];
        blk.runs.insert(i, r);
        blk.live += lu;
        if blk.runs.len() > BLOCK_MAX {
            self.split_block(b);
        }
    }

    fn split_block(&mut self, b: u32) {
        let id = self.blocks.len() as u32;
        let tail: Vec<u32> = {
            let blk = &mut self.blocks[b as usize];
            let h = blk.runs.len() / 2;
            blk.runs.split_off(h)
        };
        let mut moved = 0;
        for &r in &tail {
            self.blk[r as usize] = id;
            moved += self.live_units(r);
        }
        self.blocks[b as usize].live -= moved;
        let at = self.blocks[b as usize].pos as usize + 1;
        self.blocks.push(Block { runs: tail, live: moved, pos: at as u32 });
        self.order.insert(at, id);
        for (p, &x) in self.order.iter().enumerate().skip(at) {
            self.blocks[x as usize].pos = p as u32;
        }
    }

    /// Position (UTF-16 units of live text before it) of run r.
    fn pos_of(&self, r: u32) -> u64 {
        let b = self.blk[r as usize];
        let mut p: u64 = 0;
        for &x in &self.order[..self.blocks[b as usize].pos as usize] {
            p += self.blocks[x as usize].live;
        }
        for &x in &self.blocks[b as usize].runs {
            if x == r {
                break;
            }
            p += self.live_units(x);
        }
        p
    }

    /// The live run holding unit u (u < len16), and u's offset in it.
    fn at_unit(&self, mut u: u64) -> Option<(u32, u64)> {
        for &b in &self.order {
            let blk = &self.blocks[b as usize];
            if u >= blk.live {
                u -= blk.live;
                continue;
            }
            for &r in &blk.runs {
                let n = self.live_units(r);
                if u < n {
                    return Some((r, u));
                }
                u -= n;
            }
        }
        None
    }

    /// Character index in run r of its unit offset u (None: inside a
    /// character that takes two units).
    fn char_of_unit(&self, r: u32, u: u64) -> Option<u32> {
        let (o, n) = (self.off[r as usize] as usize, self.bytes[r as usize] as usize);
        if n == self.len[r as usize] as usize {
            return Some(u as u32);
        }
        let (mut seen_u, mut ch) = (0u64, 0u32);
        for &c in &self.text[o..o + n] {
            if c & 0xC0 == 0x80 {
                continue;
            }
            if seen_u == u {
                return Some(ch);
            }
            if seen_u > u {
                return None;
            }
            seen_u += if c >= 0xF0 { 2 } else { 1 };
            ch += 1;
        }
        if seen_u == u { Some(ch) } else { None }
    }

    // RUNS
    #[allow(clippy::too_many_arguments)]
    fn new_run(&mut self, rep: u32, ctr: u32, len: u32, parent: u64, side: u8, dead: bool, off: u32, bytes: u32, units: u32) -> u32 {
        let r = self.rep.len() as u32;
        self.rep.push(rep);
        self.ctr.push(ctr);
        self.len.push(len);
        self.prep.push((parent >> 32) as u32);
        self.pctr.push(parent as u32);
        self.side.push(side);
        self.dead.push(dead);
        self.off.push(off);
        self.bytes.push(bytes);
        self.units.push(units);
        self.blk.push(0);
        self.index.insert(key(rep, ctr), r);
        r
    }

    /// Splits run r before its character i (0 < i < len): the tail, a new
    /// run, is the right child of the head's last character.
    fn split(&mut self, r: u32, i: u32) -> u32 {
        let ru = r as usize;
        debug_assert!(0 < i && i < self.len[ru]);
        let b = self.byte_of(r, i);
        let o = self.off[ru] as usize;
        let head_units = units_of(&self.text[o..o + b as usize]);
        let (rep, ctr) = (self.rep[ru], self.ctr[ru]);
        let t = self.new_run(rep, ctr + i, self.len[ru] - i, key(rep, ctr + i - 1), RIGHT, self.dead[ru], self.off[ru] + b, self.bytes[ru] - b, self.units[ru] - head_units);
        self.len[ru] = i;
        self.bytes[ru] = b;
        self.units[ru] = head_units;
        self.kids.entry(key(rep, ctr + i - 1)).or_default().right = vec![t];
        let (blk, at) = (self.blk[ru], self.place_in_block(r));
        // (The block's live length is unchanged: put adds the tail's, so
        // take it off first.)
        let tu = self.live_units(t);
        self.blocks[blk as usize].live -= tu;
        self.put(blk, at + 1, t);
        t
    }

    /// The run starting at element k (split if k is inside one).
    fn start_at(&mut self, k: u64) -> Option<u32> {
        let (r, i) = self.find(k)?;
        Some(if i == 0 { r } else { self.split(r, i) })
    }

    /// The run ending at element k (split if k is inside one).
    fn end_at(&mut self, k: u64) -> Option<u32> {
        let (r, i) = self.find(k)?;
        if i + 1 < self.len[r as usize] {
            self.split(r, i + 1);
        }
        Some(r)
    }

    /// The first run of the subtree of run r's first element (in order).
    fn leftmost(&self, mut r: u32) -> u32 {
        while let Some(&c) = self.kids.get(&self.first(r)).and_then(|k| k.left.first()) {
            r = c;
        }
        r
    }

    /// Where a new run whose first element has key nk goes, as the side
    /// child of element p (p a run start if side is LEFT, a run end if
    /// RIGHT, or the root).
    fn place(&self, nk: u64, p: u64, side: u8) -> Place {
        if let Some(k) = self.kids.get(&p) {
            let sibs = if side == LEFT { &k.left } else { &k.right };
            let i = sibs.partition_point(|&s| self.first(s) < nk);
            if let Some(&next) = sibs.get(i) {
                return Place::Before(self.leftmost(next));
            }
        }
        if side == LEFT {
            let (r, _) = self.find(p).expect("parent checked");
            return Place::Before(r);
        }
        // After the whole subtree of p: its rightmost element.
        let mut x = p;
        while let Some(&c) = self.kids.get(&x).and_then(|k| k.right.last()) {
            x = self.last(c);
        }
        if x == ROOT {
            return Place::First;
        }
        Place::After(self.find(x).expect("an element").0)
    }

    // OPERATIONS
    /// Checks an operation against this document without changing it.
    fn check(&self, op: &Op) -> Result<(), Bad> {
        match *op {
            Op::Ins { rep, ctr, parent, side, text } => {
                let n = text.chars().count() as u64;
                if rep == 0 || ctr == 0 || ctr as u64 + n - 1 > u32::MAX as u64 {
                    return Err(Bad::Id);
                }
                // No element of the run may exist yet.
                let (a, z) = (key(rep, ctr), key(rep, ctr + (n - 1) as u32));
                if self.find(a).is_some() || self.index.range(a..=z).next().is_some() {
                    return Err(Bad::Id);
                }
                if parent == ROOT {
                    if side != RIGHT {
                        return Err(Bad::Missing);
                    }
                } else if self.find(parent).is_none() {
                    return Err(Bad::Missing);
                }
                Ok(())
            }
            Op::Del { rep, ctr, len } => {
                if rep == 0 || ctr == 0 || ctr as u64 + len as u64 - 1 > u32::MAX as u64 {
                    return Err(Bad::Id);
                }
                let (mut c, end) = (ctr as u64, ctr as u64 + len as u64);
                while c < end {
                    let (r, i) = self.find(key(rep, c as u32)).ok_or(Bad::Missing)?;
                    c += (self.len[r as usize] - i) as u64;
                }
                Ok(())
            }
        }
    }

    /// Applies one operation (checked first; refused whole if bad).
    pub fn apply(&mut self, op: &Op, sink: &mut dyn Sink) -> Result<(), Bad> {
        self.check(op)?;
        match *op {
            Op::Ins { rep, ctr, parent, side, text } => self.insert(rep, ctr, parent, side, text, sink),
            Op::Del { rep, ctr, len } => self.delete(rep, ctr, len, sink),
        }
        Ok(())
    }

    /// Applies a batch; stops at the first bad operation (the ones before
    /// it stay applied: a caller that must refuse the batch whole checks it
    /// first with a copy, or, as the server does, applies to its own copy
    /// and stores only good batches).
    pub fn apply_batch(&mut self, b: &[u8], sink: &mut dyn Sink) -> Result<usize, Bad> {
        let (mut i, mut n) = (0, 0);
        while i < b.len() {
            let (op, next) = decode(b, i)?;
            self.apply(&op, sink)?;
            i = next;
            n += 1;
        }
        Ok(n)
    }

    fn insert(&mut self, rep: u32, ctr: u32, parent: u64, side: u8, text: &str, sink: &mut dyn Sink) {
        if parent != ROOT {
            if side == RIGHT {
                self.end_at(parent);
            } else {
                self.start_at(parent);
            }
        }
        let n = text.chars().count() as u32;
        let units = units_of(text.as_bytes());
        let place = self.place(key(rep, ctr), parent, side);
        let off = self.text.len() as u32;
        self.text.extend_from_slice(text.as_bytes());
        self.live += units as u64;
        self.chars += n as u64;
        // Typing: the run the parent ends continues (same replica, next
        // counter, its text last in the buffer).
        if let Place::After(r) = place {
            let ru = r as usize;
            if side == RIGHT
                && self.last(r) == parent
                && self.rep[ru] == rep
                && self.ctr[ru] + self.len[ru] == ctr
                && !self.dead[ru]
                && self.off[ru] + self.bytes[ru] == off
            {
                // (The parent has no right child: else the place would be
                // after its subtree's rightmost element, not after it.)
                let at = self.pos_of(r) + self.units[ru] as u64;
                self.len[ru] += n;
                self.bytes[ru] += text.len() as u32;
                self.units[ru] += units;
                self.blocks[self.blk[ru] as usize].live += units as u64;
                sink.change(at, 0, text);
                return;
            }
        }
        let t = self.new_run(rep, ctr, n, parent, side, false, off, text.len() as u32, units);
        match place {
            Place::First => {
                let b = self.order[0];
                self.put(b, 0, t);
            }
            Place::Before(x) => {
                let (b, i) = (self.blk[x as usize], self.place_in_block(x));
                self.put(b, i, t);
            }
            Place::After(x) => {
                let (b, i) = (self.blk[x as usize], self.place_in_block(x));
                self.put(b, i + 1, t);
            }
        }
        let tk = key(rep, ctr);
        let k = self.kids.entry(parent).or_default();
        let sibs = if side == LEFT { &mut k.left } else { &mut k.right };
        // (Ascending by first element; the new one's place among them.)
        let (reps, ctrs) = (&self.rep, &self.ctr);
        let i = sibs.partition_point(|&s| key(reps[s as usize], ctrs[s as usize]) < tk);
        sibs.insert(i, t);
        sink.change(self.pos_of(t), 0, text);
    }

    fn delete(&mut self, rep: u32, ctr: u32, len: u32, sink: &mut dyn Sink) {
        let (mut c, end) = (ctr as u64, ctr as u64 + len as u64);
        while c < end {
            let r = self.start_at(key(rep, c as u32)).expect("checked");
            let ru = r as usize;
            let take = (end - c).min(self.len[ru] as u64) as u32;
            if take < self.len[ru] {
                self.split(r, take);
            }
            if !self.dead[ru] {
                let at = self.pos_of(r);
                let u = self.units[ru] as u64;
                self.dead[ru] = true;
                self.blocks[self.blk[ru] as usize].live -= u;
                self.live -= u;
                self.chars -= take as u64;
                sink.change(at, u, "");
            }
            c += take as u64;
        }
    }

    // LOCAL EDITS
    /// The element whose last unit is unit u - 1 (the character just
    /// before position u), or the root for u = 0.
    fn before(&self, u: u64) -> Result<u64, Bad> {
        if u == 0 {
            return Ok(ROOT);
        }
        let (r, o) = self.at_unit(u - 1).ok_or(Bad::Position)?;
        // The character ending at unit u: its index is that of position u
        // in the run, minus one.
        let i = self.char_of_unit(r, o + 1).ok_or(Bad::Position)?;
        Ok(key(self.rep[r as usize], self.ctr[r as usize] + i - 1))
    }

    /// Replaces del units at position pos with ins, as replica rep: the
    /// operations are applied here and appended to out.
    pub fn edit(&mut self, rep: u32, pos: u64, del: u64, ins: &str, out: &mut Vec<u8>) -> Result<(), Bad> {
        if rep == 0 || pos.checked_add(del).is_none_or(|e| e > self.live) {
            return Err(Bad::Position);
        }
        // Checked before anything changes: the character before pos (the
        // same after the deletion, which is after it) and the counters.
        if !ins.is_empty() {
            self.before(pos)?;
            let n = ins.chars().count() as u64;
            if self.next_ctr(rep) as u64 + n - 1 > u32::MAX as u64 {
                return Err(Bad::Id);
            }
        }
        if del > 0 {
            // The live elements in [pos, pos + del), as id ranges.
            let mut dels: Vec<(u32, u32, u32)> = vec![];
            let (mut u, mut left) = (pos, del);
            while left > 0 {
                let (r, o) = self.at_unit(u).ok_or(Bad::Position)?;
                let ru = r as usize;
                let a = self.char_of_unit(r, o).ok_or(Bad::Position)?;
                let avail = self.units[ru] as u64 - o;
                let take_u = left.min(avail);
                let b = self.char_of_unit(r, o + take_u).ok_or(Bad::Position)?;
                dels.push((self.rep[ru], self.ctr[ru] + a, b - a));
                u += take_u;
                left -= take_u;
            }
            for (rep_d, ctr_d, n) in dels {
                let op = Op::Del { rep: rep_d, ctr: ctr_d, len: n };
                encode(&op, out);
                self.delete(rep_d, ctr_d, n, &mut ());
            }
        }
        if !ins.is_empty() {
            let l = self.before(pos)?;
            // A right child of the element before, if it has none (the new
            // run goes right after it); else a left child of the element
            // right after it in the order (a tombstone, perhaps), which then
            // has none: the same place.
            let has_right = if l == ROOT {
                self.kids.get(&ROOT).is_some_and(|k| !k.right.is_empty())
            } else {
                let (r, i) = self.find(l).expect("an element");
                i + 1 < self.len[r as usize] || self.kids.get(&l).is_some_and(|k| !k.right.is_empty())
            };
            let (parent, side) = if !has_right {
                (l, RIGHT)
            } else if l == ROOT {
                let b = self.order.iter().find(|&&b| !self.blocks[b as usize].runs.is_empty()).expect("a right child exists");
                (self.first(self.blocks[*b as usize].runs[0]), LEFT)
            } else {
                let (r, i) = self.find(l).expect("an element");
                if i + 1 < self.len[r as usize] {
                    (l + 1, LEFT)
                } else {
                    (self.first(self.next_run(r)), LEFT)
                }
            };
            let ctr = self.next_ctr(rep);
            let op = Op::Ins { rep, ctr, parent, side, text: ins };
            encode(&op, out);
            self.insert(rep, ctr, parent, side, ins, &mut ());
        }
        Ok(())
    }

    /// The run after r in document order (r must have one).
    fn next_run(&self, r: u32) -> u32 {
        let b = self.blk[r as usize];
        let i = self.place_in_block(r);
        if let Some(&x) = self.blocks[b as usize].runs.get(i + 1) {
            return x;
        }
        for &nb in &self.order[self.blocks[b as usize].pos as usize + 1..] {
            if let Some(&x) = self.blocks[nb as usize].runs.first() {
                return x;
            }
        }
        panic!("no run after")
    }

    // SNAPSHOTS: every run in document order, so loading is one pass with
    // no merging: rep, ctr, len, parent rep, parent ctr, side, dead (u8),
    // byte length, text (u32s little-endian). Tests check load(save(d)) is d.
    pub fn save(&self, out: &mut Vec<u8>) {
        out.extend_from_slice(b"FUG1");
        out.extend_from_slice(&(self.rep.len() as u32).to_le_bytes());
        for &b in &self.order {
            for &r in &self.blocks[b as usize].runs {
                let ru = r as usize;
                for v in [self.rep[ru], self.ctr[ru], self.len[ru], self.prep[ru], self.pctr[ru]] {
                    out.extend_from_slice(&v.to_le_bytes());
                }
                out.push(self.side[ru]);
                out.push(self.dead[ru] as u8);
                out.extend_from_slice(&self.bytes[ru].to_le_bytes());
                let o = self.off[ru] as usize;
                out.extend_from_slice(&self.text[o..o + self.bytes[ru] as usize]);
            }
        }
    }

    /// A document from a snapshot (checked: ids unique, parents present,
    /// lengths and text consistent). Its structure is rebuilt as saved.
    pub fn load(b: &[u8]) -> Result<Doc, Bad> {
        let f = Bad::Format;
        if b.get(..4) != Some(b"FUG1") {
            return Err(f);
        }
        let n = u32_at(b, 4).ok_or(f)? as usize;
        let mut d = Doc::new();
        let mut i = 8;
        for _ in 0..n {
            let v = |j: usize| u32_at(b, i + 4 * j).ok_or(f);
            let (rep, ctr, len, prep, pctr) = (v(0)?, v(1)?, v(2)?, v(3)?, v(4)?);
            let side = *b.get(i + 20).ok_or(f)?;
            let dead = *b.get(i + 21).ok_or(f)?;
            let nb = u32_at(b, i + 22).ok_or(f)? as usize;
            let s = i + 26;
            let t = std::str::from_utf8(b.get(s..s.checked_add(nb).ok_or(f)?).ok_or(f)?).map_err(|_| f)?;
            let chars = t.chars().count() as u64;
            if side > 1 || dead > 1 || len == 0 || chars != len as u64 || rep == 0 || ctr == 0 || ctr as u64 + len as u64 - 1 > u32::MAX as u64 {
                return Err(f);
            }
            if d.index.range(key(rep, ctr)..=key(rep, ctr + len - 1)).next().is_some() || d.find(key(rep, ctr)).is_some() {
                return Err(Bad::Id);
            }
            let off = d.text.len() as u32;
            d.text.extend_from_slice(t.as_bytes());
            let units = units_of(t.as_bytes());
            let parent = key(prep, pctr);
            let r = d.new_run(rep, ctr, len, parent, side, dead == 1, off, nb as u32, units);
            if dead == 0 {
                d.live += units as u64;
                d.chars += len as u64;
            }
            // Appended in order: to the last block (a new one when full).
            let mut last = *d.order.last().expect("a block");
            if d.blocks[last as usize].runs.len() >= BLOCK_MAX {
                last = d.blocks.len() as u32;
                d.blocks.push(Block { runs: vec![], live: 0, pos: d.order.len() as u32 });
                d.order.push(last);
            }
            let at = d.blocks[last as usize].runs.len();
            d.put(last, at, r);
            i = s + nb;
        }
        if i != b.len() {
            return Err(f);
        }
        // The tree: each run a child of its parent, siblings ascending.
        for r in 0..d.rep.len() as u32 {
            let p = key(d.prep[r as usize], d.pctr[r as usize]);
            if p != ROOT && d.find(p).is_none() {
                return Err(Bad::Missing);
            }
            let side = d.side[r as usize];
            let k = d.kids.entry(p).or_default();
            if side == LEFT { k.left.push(r) } else { k.right.push(r) }
        }
        let (reps, ctrs) = (&d.rep, &d.ctr);
        for k in d.kids.values_mut() {
            k.left.sort_unstable_by_key(|&s| key(reps[s as usize], ctrs[s as usize]));
            k.right.sort_unstable_by_key(|&s| key(reps[s as usize], ctrs[s as usize]));
        }
        // A saved document is in the order its tree gives; a doctored one
        // is refused (else two replicas could disagree).
        if !d.order_ok() {
            return Err(f);
        }
        Ok(d)
    }

    /// The tree's in-order walk equals the stored order (for load, tests).
    pub fn order_ok(&self) -> bool {
        let mut walk: Vec<u32> = Vec::with_capacity(self.rep.len());
        // Explicit stack of (run, stage): 0 = its left kids next, 1 = the
        // run then its right kids. A run's in-order walk: left kids of its
        // first element, the run, then right kids of its last element
        // (interior elements have no kids).
        enum F {
            Run(u32),
            Emit(u32),
        }
        let mut stack: Vec<F> = vec![];
        if let Some(k) = self.kids.get(&ROOT) {
            if !k.left.is_empty() {
                return false;
            }
            for &c in k.right.iter().rev() {
                stack.push(F::Run(c));
            }
        }
        while let Some(f) = stack.pop() {
            match f {
                F::Run(r) => {
                    let k0 = self.kids.get(&self.first(r));
                    let kl = self.kids.get(&self.last(r));
                    for &c in kl.map_or(&[][..], |k| &k.right[..]).iter().rev() {
                        stack.push(F::Run(c));
                    }
                    stack.push(F::Emit(r));
                    for &c in k0.map_or(&[][..], |k| &k.left[..]).iter().rev() {
                        stack.push(F::Run(c));
                    }
                }
                F::Emit(r) => walk.push(r),
            }
            if walk.len() > self.rep.len() {
                return false;
            }
        }
        let stored: Vec<u32> = self.order.iter().flat_map(|&b| self.blocks[b as usize].runs.iter().copied()).collect();
        // Interior elements with kids would be skipped by the walk above:
        // then the walk is shorter than the stored order.
        walk == stored
    }
}
