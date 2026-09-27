// A post's text as the server stores it (for tests/large_test.py): the
// snapshot and the batches after it, applied by the same CRDT the server
// runs (build/app.wasm, src/crdt.rs), as the server's Docs::get does.
// usage: node tests/large_test_text.mjs FILE > text
// FILE: records of kind (u8: 0 snapshot, 1 batch), length (u32 LE), bytes.
import { readFileSync, writeSync } from "node:fs";

const root = new URL("..", import.meta.url).pathname;
const { instance } = await WebAssembly.instantiate(readFileSync(root + "build/app.wasm"), {});
const w = instance.exports;
const put = (bytes) => {
  const p = w.input(bytes.length);
  new Uint8Array(w.memory.buffer, p, bytes.length).set(bytes);
};
const buf = readFileSync(process.argv[2]);
w.reset();
for (let at = 0; at < buf.length; ) {
  const kind = buf[at];
  const len = buf.readUInt32LE(at + 1);
  put(buf.subarray(at + 5, at + 5 + len));
  const r = kind === 0 ? w.load() : w.apply();
  if (r < 0) {
    console.error("bad " + (kind === 0 ? "snapshot" : "batch") + ": " + r);
    process.exit(1);
  }
  at += 5 + len;
}
w.text();
const out = new Uint8Array(w.memory.buffer, w.output(), w.output_len());
for (let i = 0; i < out.length; ) i += writeSync(1, out, i, out.length - i);
