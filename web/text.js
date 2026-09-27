// A long text as chunks of a few KB, for the editor's view and Vim.
//
// A JavaScript string of a novel (6M characters, 12 MB) costs a copy of
// all of it on every edit: the engine builds the new string lazily, and
// the next search or slice makes it whole again (measured: ~6 ms a key at
// 6M, plus the garbage). Here an edit changes the chunks it touches and
// the table of where each starts; everything else is shared with the text
// before. Like a string, a Text is a value: with() makes the edited copy
// (Vim, the undo history and the view keep earlier texts and compare them).
//
// It has the few string methods the editor uses (length, charAt,
// charCodeAt, codePointAt, slice, indexOf, lastIndexOf, endsWith,
// toString), with the same meaning, so code written for strings (Vim,
// diff, the undo history) takes either. Positions are UTF-16 units.

export class Text {
  // Units a chunk is cut at (tests set it small, to put seams everywhere).
  static CHUNK = 8192;

  constructor(s = "") {
    this.chunks = [];
    this.starts = [];
    this.n = 0;
    if (s !== "") this.chunks = cut(s);
    this.index(0);
  }

  get length() {
    return this.n;
  }

  // Where each chunk starts, from chunk k on (and the length).
  index(k) {
    const c = this.chunks;
    this.starts.length = c.length;
    let s = k > 0 ? this.starts[k - 1] + c[k - 1].length : 0;
    for (let i = k; i < c.length; i++) {
      this.starts[i] = s;
      s += c[i].length;
    }
    this.n = s;
  }

  // The chunk holding position i (for i = length: the last chunk).
  find(i) {
    const st = this.starts;
    let lo = 0, hi = st.length - 1;
    while (lo < hi) {
      const m = (lo + hi + 1) >> 1;
      if (st[m] <= i) lo = m;
      else hi = m - 1;
    }
    return lo;
  }

  charCodeAt(i) {
    if (!(i >= 0 && i < this.n)) return NaN;
    const k = this.find(i);
    return this.chunks[k].charCodeAt(i - this.starts[k]);
  }

  charAt(i) {
    if (!(i >= 0 && i < this.n)) return "";
    const k = this.find(i);
    return this.chunks[k].charAt(i - this.starts[k]);
  }

  codePointAt(i) {
    const a = this.charCodeAt(i);
    if (a >= 0xd800 && a <= 0xdbff && i + 1 < this.n) {
      const b = this.charCodeAt(i + 1);
      if (b >= 0xdc00 && b <= 0xdfff) return (a - 0xd800) * 0x400 + (b - 0xdc00) + 0x10000;
    }
    return Number.isNaN(a) ? undefined : a;
  }

  slice(a = 0, b = this.n) {
    const n = this.n;
    a = a < 0 ? Math.max(0, n + a) : Math.min(a, n);
    b = b < 0 ? Math.max(0, n + b) : Math.min(b, n);
    if (b <= a) return "";
    let k = this.find(a);
    const e = this.find(b - 1);
    if (k === e) return this.chunks[k].slice(a - this.starts[k], b - this.starts[k]);
    const out = [this.chunks[k].slice(a - this.starts[k])];
    for (k++; k < e; k++) out.push(this.chunks[k]);
    out.push(this.chunks[e].slice(0, b - this.starts[e]));
    return out.join("");
  }

  // The first s at or after from (String.prototype.indexOf's meaning).
  indexOf(s, from = 0) {
    from = Math.max(0, Math.min(from | 0, this.n));
    if (s === "") return from;
    const c = this.chunks, over = s.length - 1;
    for (let k = from < this.n ? this.find(from) : c.length; k < c.length; k++) {
      // (The chunk and enough of the next ones to find s across the seam.)
      let piece = c[k];
      for (let j = k + 1; j < c.length && piece.length < c[k].length + over; j++) piece += c[j].slice(0, over - (piece.length - c[k].length));
      const i = piece.indexOf(s, Math.max(0, from - this.starts[k]));
      if (i >= 0 && i < c[k].length) return this.starts[k] + i;
    }
    return -1;
  }

  // The last s starting at or before from (String.prototype.lastIndexOf's).
  lastIndexOf(s, from = Infinity) {
    const n = this.n;
    from = Number.isNaN(+from) ? n : Math.max(0, Math.min(Math.floor(+from), n));
    if (s === "") return from;
    if (n === 0) return -1;
    const c = this.chunks, over = s.length - 1;
    for (let k = this.find(Math.min(from, n - 1)); k >= 0; k--) {
      let piece = c[k];
      for (let j = k + 1; j < c.length && piece.length < c[k].length + over; j++) piece += c[j].slice(0, over - (piece.length - c[k].length));
      // (Starting in this chunk: later ones were searched already.)
      const i = piece.lastIndexOf(s, Math.min(from - this.starts[k], c[k].length - 1));
      if (i >= 0 && i < c[k].length) return this.starts[k] + i;
    }
    return -1;
  }

  endsWith(s) {
    return s.length <= this.n && this.slice(this.n - s.length) === s;
  }

  toString() {
    return this.chunks.join("");
  }

  // [p, p + del) becomes ins, in place (only on a Text no one else holds:
  // with() uses it on its fresh copy).
  splice(p, del, ins) {
    const n = this.n;
    p = Math.max(0, Math.min(p, n));
    del = Math.max(0, Math.min(del, n - p));
    if (del === 0 && ins === "") return this;
    if (this.chunks.length === 0) {
      this.chunks = cut(ins);
      this.index(0);
      return this;
    }
    const k0 = this.find(p);
    const k1 = del > 0 ? this.find(p + del - 1) : k0;
    const s0 = this.starts[k0], s1 = this.starts[k1];
    const merged = this.chunks[k0].slice(0, p - s0) + ins + this.chunks[k1].slice(p + del - s1);
    const parts = merged.length > 2 * Text.CHUNK ? cut(merged) : merged === "" ? [] : [merged];
    this.chunks.splice(k0, k1 - k0 + 1, ...parts);
    // (A chunk left tiny joins the one before, so chunks stay few.)
    const k = Math.max(0, k0 - 1);
    if (k0 > 0 && k0 < this.chunks.length && this.chunks[k0].length < Text.CHUNK / 8 && this.chunks[k].length + this.chunks[k0].length <= 2 * Text.CHUNK) {
      this.chunks.splice(k, 2, this.chunks[k] + this.chunks[k0]);
    }
    this.index(Math.min(k, this.chunks.length));
    return this;
  }

  // A copy with [a, b) replaced by x (the chunks not touched are shared).
  // Nothing to change: this text itself.
  with(a, b, x) {
    if (a >= b && x === "") return this;
    const t = new Text();
    t.chunks = this.chunks.slice();
    t.starts = this.starts.slice();
    t.n = this.n;
    return t.splice(a, b - a, x);
  }
}

function cut(s) {
  const out = [];
  const n = Text.CHUNK;
  for (let i = 0; i < s.length; i += n) out.push(s.slice(i, i + n));
  return out;
}
