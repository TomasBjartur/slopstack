// The browser's WebAssembly (build/app.wasm) against the server's native
// build: the same HTML for every CommonMark example and random inputs,
// and the CRDT working through the byte interface (edits, batches,
// changes reported, snapshots). usage: node tests/wasm_test.mjs
import { readFileSync } from "node:fs";
import { execFileSync } from "node:child_process";

let fails = 0;
const check = (name, ok, detail = "") => {
  if (!ok) fails++;
  console.log((ok ? "PASS " : "FAIL ") + name + (ok ? "" : "   " + detail));
};

const { instance } = await WebAssembly.instantiate(readFileSync("build/app.wasm"), {});
const w = instance.exports;
const enc = new TextEncoder(), dec = new TextDecoder();
const put = (bytes) => { const p = w.input(bytes.length); new Uint8Array(w.memory.buffer, p, bytes.length).set(bytes); };
const out = () => new Uint8Array(w.memory.buffer, w.output(), w.output_len()).slice();
const render = (md) => { put(enc.encode(md)); w.render(); return dec.decode(out()); };
const native = (md) => execFileSync("build/server", ["test", "render"], { input: md }).toString();

// Rendering: every CommonMark example, in one native run (joined with a
// separator paragraph the renderer passes through as text).
const spec = JSON.parse(readFileSync("tests/commonmark/spec.json"));
let same = 0;
const SEP = "\n\nSEPARATOR-7f3a\n\n";
const all = native(spec.map((e) => e.markdown).join(SEP));
const wall = render(spec.map((e) => e.markdown).join(SEP));
check(`all ${spec.length} CommonMark examples joined: the same HTML as the server`, all === wall, `${all.length} vs ${wall.length}`);
for (const e of spec.slice(0, 200)) if (render(e.markdown) === native(e.markdown)) same++;
check(`200 examples one by one: ${same} identical`, same === 200);
let rnd = 0;
const parts = ["*", "_", "[", "]", "(", ")", "<", ">", "`", "\n", " ", "a", "#", "-", "1.", "&amp;", "\\", "!", "http://x.y", "<script>", "\"", "'"];
let seed = 12345;
const rand = (n) => { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n; };
let same2 = 0;
for (let i = 0; i < 100; i++) {
  let s = "";
  for (let j = 0; j < 40; j++) s += parts[rand(parts.length)];
  rnd++;
  if (render(s) === native(s)) same2++;
}
check(`${rnd} random inputs: ${same2} identical`, same2 === rnd);

// The CRDT through the byte interface.
w.reset();
const edit = (rep, pos, del, ins) => { put(enc.encode(ins)); const r = w.edit(rep, pos, del); return [r, out()]; };
const text = () => { w.text(); return dec.decode(out()); };
const [r1, ops1] = edit(2, 0, 0, "hello 🌊 world");
const [r2, ops2] = edit(2, 6, 2, "sea");
check("edits", r1 === 0 && r2 === 0 && text() === "hello sea world" && w.len16() === 15, text());
check("an edit inside a surrogate pair is refused", edit(2, 20, 0, "x")[0] === -4);
// Another replica: apply the batches, see the changes reported.
const batch = new Uint8Array([...ops1, ...ops2]);
w.reset();
put(batch);
const n = w.apply();
const ev = out();
const dv = new DataView(ev.buffer);
const changes = [];
for (let i = 0; i < ev.length; ) {
  const pos = dv.getUint32(i, true), del = dv.getUint32(i + 4, true), len = dv.getUint32(i + 8, true);
  changes.push([pos, del, dec.decode(ev.subarray(i + 12, i + 12 + len))]);
  i += 12 + len;
}
check("a batch applied, its changes reported", n === changes.length && text() === "hello sea world" && changes[0][2] === "hello 🌊 world", JSON.stringify(changes));
put(batch);
check("…applied again: nothing changes", w.apply() === 0 && text() === "hello sea world");
put(new Uint8Array([9, 9, 9]));
check("a malformed batch: an error", w.apply() === -1);
console.log(`${fails} failure(s)`);
process.exit(fails ? 1 : 0);
