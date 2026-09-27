// The browser's half of the document and the renderer: build/app.wasm
// (src/wasm.rs), the same Rust as the server's CRDT (src/crdt.rs) and
// Markdown renderer (src/markdown.rs with the proved markup builder,
// src/html.rs). Checked against the server's build by tests/wasm_test.mjs.
//
// ready() must resolve before anything else here is used (the editor
// awaits it; the page's text is read-only until then).
let w = null;
const enc = new TextEncoder();
const dec = new TextDecoder();

export async function ready(url) {
  if (w) return;
  const res = await fetch(url, { credentials: "same-origin" });
  const got = WebAssembly.instantiateStreaming
    ? await WebAssembly.instantiateStreaming(res, {})
    : await WebAssembly.instantiate(await res.arrayBuffer(), {});
  w = got.instance.exports;
}

/** Bytes of WebAssembly memory (the document lives there; tests weigh it). */
export function memoryBytes() {
  return w ? w.memory.buffer.byteLength : 0;
}

function put(bytes) {
  const p = w.input(bytes.length); // (may grow memory: take the buffer after)
  new Uint8Array(w.memory.buffer, p, bytes.length).set(bytes);
}

function out() {
  return new Uint8Array(w.memory.buffer, w.output(), w.output_len());
}

/** Markdown to allowed markup (the server publishes exactly this). */
export function render(md) {
  put(enc.encode(md));
  w.render();
  return dec.decode(out());
}

const ERRORS = { "-1": "malformed", "-2": "id", "-3": "missing", "-4": "position" };

/** The document (one a page). Positions are UTF-16 units. */
export class Doc {
  constructor() {
    w.reset();
  }

  get length() {
    return w.len16();
  }

  /** A snapshot from the server. */
  load(bytes) {
    put(bytes);
    const r = w.load();
    if (r < 0) throw new Error("snapshot: " + ERRORS[r]);
  }

  /** A local edit; its operations (bytes to send). */
  edit(rep, pos, del, ins) {
    put(enc.encode(ins));
    const r = w.edit(rep, pos, del);
    if (r < 0) throw new Error("edit: " + ERRORS[r]);
    return out().slice();
  }

  /** A batch from elsewhere. Each visible change goes to sink(pos, del,
   * ins). Answers 0 (nothing changed), 1 (changes reported) or 2 (too many
   * to report: take text()). Throws on a bad batch. */
  apply(bytes, sink = null) {
    put(bytes);
    const n = w.apply();
    if (n < 0) throw new Error("batch: " + ERRORS[n]);
    if (n === 0) return 0;
    if (n > 2000) return 2;
    if (sink) {
      const ev = out().slice();
      const dv = new DataView(ev.buffer);
      for (let i = 0; i < ev.length; ) {
        const pos = dv.getUint32(i, true), del = dv.getUint32(i + 4, true), len = dv.getUint32(i + 8, true);
        sink(pos, del, dec.decode(ev.subarray(i + 12, i + 12 + len)));
        i += 12 + len;
      }
    }
    return 1;
  }

  text() {
    w.text();
    return dec.decode(out());
  }
}
