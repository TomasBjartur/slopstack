// web/text.js (the editor's chunked text) against plain strings: random
// edits and every method, at chunk seams. usage: node tests/text_test.mjs [n]
import { Text } from "../web/text.js";
import { prefix, suffix } from "../web/common.js";

let seed = 7;
const rnd = (n) => ((seed = (seed * 1103515245 + 12345) >>> 0) >>> 8) % n;
const alpha = ["a", "b", "\n", " ", "ab", "😀", "x".repeat(5000), "y".repeat(20000)];
const word = () => { let s = ""; for (let i = rnd(4); i >= 0; i--) s += alpha[rnd(alpha.length)]; return s; };
let fails = 0;
const eq = (what, a, b) => { if (!Object.is(a, b)) { if (fails++ < 10) console.log("FAIL", what, String(a).slice(0, 40), String(b).slice(0, 40)); } };

const rounds = +(process.argv[2] || 3000);
for (let run = 0; run < 20; run++) {
  let s = rnd(2) ? "" : "z".repeat(rnd(40000));
  let t = new Text(s);
  for (let r = 0; r < rounds / 20; r++) {
    const p = rnd(s.length + 1), del = rnd(3) ? rnd(Math.min(50, s.length - p + 1)) : rnd(s.length - p + 1), ins = rnd(4) ? word() : "";
    const before = t, was = s;
    t = t.with(p, p + del, ins);
    s = s.slice(0, p) + ins + s.slice(p + del);
    if (rnd(10) === 0) eq("with() leaves the text before as it was", before.toString(), was);
    const pf = prefix(before, t);
    eq("prefix of two Texts", pf, prefix(was, s));
    eq("suffix of two Texts", suffix(before, t, pf), suffix(was, s, pf));
    eq("prefix of a Text and a string", prefix(t, was), prefix(s, was));
    eq("length", t.length, s.length);
    for (let q = 0; q < 6; q++) {
      const i = rnd(s.length + 2) - 1, j = rnd(s.length + 2) - 1;
      eq("charAt", t.charAt(i), s.charAt(i));
      eq("charCodeAt", t.charCodeAt(i), s.charCodeAt(i));
      eq("codePointAt", t.codePointAt(i), s.codePointAt(i));
      eq("slice", t.slice(i, j), s.slice(i, j));
      eq("slice1", t.slice(i), s.slice(i));
      const pat = rnd(2) ? s.slice(i, i + 1 + rnd(8)) : ["\n", "ab", "b\na", "xy", "zz"][rnd(5)];
      eq("indexOf " + JSON.stringify(pat), t.indexOf(pat, j), s.indexOf(pat, j));
      eq("lastIndexOf " + JSON.stringify(pat), t.lastIndexOf(pat, j), s.lastIndexOf(pat, j));
      eq("lastIndexOf()", t.lastIndexOf(pat), s.lastIndexOf(pat));
      eq("endsWith", t.endsWith(pat), s.endsWith(pat));
    }
  }
  eq("toString", t.toString(), s);
  eq("chunks", t.chunks.every((c) => c.length > 0 && c.length <= 16384 + 20000 * 4), true);
}
// A search back from just inside a chunk, with a match just after: the
// match before it, in the chunk before, is still found.
{
  const s = "abc" + "-".repeat(8189) + "-abc" + "-".repeat(100);
  const t = new Text(s);
  eq("lastIndexOf across a seam", t.lastIndexOf("abc", 8192), s.lastIndexOf("abc", 8192));
  eq("indexOf across a seam", t.indexOf("-ab", 8000), s.indexOf("-ab", 8000));
}
// An edit at 6M characters touches a chunk, not the text.
const big = new Text("w".repeat(6e6));
let t0 = performance.now();
let cur = big;
for (let i = 0; i < 1000; i++) cur = cur.with(3e6 + i, 3e6 + i, "k");
const per = (performance.now() - t0) / 1000;
t0 = performance.now();
for (let i = 0; i < 100; i++) prefix(big, cur), suffix(big, cur, 0);
console.log(`prefix and suffix of two Texts at 6M: ${((performance.now() - t0) * 10).toFixed(1)} us`);
eq("with() and the texts before it", big.length === 6e6 && cur.length === 6e6 + 1000, true);
console.log(`with() at 6M: ${(per * 1000).toFixed(1)} us, chunks ${big.chunks.length}`);
eq("with() at 6M under 0.5 ms", per < 0.5, true);
console.log(fails ? `${fails} failure(s)` : "PASS text");
process.exit(fails ? 1 : 0);
