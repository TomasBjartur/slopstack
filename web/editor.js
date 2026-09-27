// The collaborative editor (local-first: the browser holds the document
// and merges; the server stores operations and hands them on). Local
// edits become CRDT operations at once; a sync loop exchanges them with
// the server; unsent operations are kept in localStorage, so edits made
// offline are sent when back online.
//
// The parts (each built for documents of any length, a novel included):
// - web/crdt.js: the document and the renderer, the server's own Rust
//   compiled to WebAssembly (src/crdt.rs, src/markdown.rs).
// - web/view.js: the Markdown view (a textarea window over the text).
// - web/history.js: undo and redo of this writer's own changes.
// - web/modes.js: Visual mode and Vim keybindings.
// Positions are UTF-16 units everywhere.
//
// SYNC (POST /edit/<id>/sync, src/site.rs): body = since (u64), rep (u32),
// operations; answer = kind (1: a snapshot first), seq (u64), more (u8),
// [snapshot length u32, snapshot], operations.
import { Doc, ready } from "./crdt.js";
import { View, diff } from "./view.js";
import { History } from "./history.js";
import { setupModes } from "./modes.js";

const SYNC_MS = 1500;       // polling while nothing happens
const FAST_MS = 400;        // polling while others are typing
const DEBOUNCE_MS = 150;    // after a local edit
// A request carries at most this much (the server takes 16 MiB); a larger
// paste is cut into pieces of at most PIECE characters, each its own
// operation, and sent over several requests.
const SEND_BYTES = 4 << 20;
const PIECE = 1 << 20;
// The unsent operations are copied to localStorage at most this often
// (and when the page is hidden), not on every keystroke.
const STORE_MS = 1000;

const ta = document.getElementById("editor");
if (ta) start(ta);

function start(ta) {
  const post = ta.dataset.post;
  const rep = Number(ta.dataset.rep);
  const url = "/edit/" + post + "/sync";
  const key = "slop:post:" + post;
  const status = document.getElementById("sync-status");
  // A published post's edits are saved as you type but only go live with
  // Update: the status says so, rather than a "Saved" that reads as live.
  const live = ta.dataset.published === "1";
  let edited = false;
  let saveLabel = "Saved";
  const savedText = () => (live && edited ? "Saved · press Update to publish changes" : saveLabel);

  let doc = null;           // every operation known here (after ready())
  let since = 0;            // the last stored batch seen
  let batches = [];         // local batches not yet acknowledged: {rep, ops}, oldest first
  let busy = false;
  let timer = 0;
  let fast = 0;             // time of the last change from elsewhere
  const history = new History();
  // Until the document has arrived, the view shows the page's text (or
  // nothing, for a long one) but the CRDT does not hold it yet: an edit
  // then would be made against a partial document, so it is read-only.
  let loaded = false;

  // The textarea's value is now the CRDT's: the form saves the title only.
  ta.removeAttribute("name");
  const view = new View(ta, { edit: typed });
  view.readOnly = true;
  // For tests and debugging (the whole text is not in the textarea).
  ta.ed = { view, doc: () => doc, history, loaded: () => loaded, pending: () => batches.length };

  restore();

  const form = ta.form;
  const title = form && form.elements.namedItem("title");
  // A new draft is stored as "Untitled": show an empty title with its
  // placeholder instead, and send "Untitled" if it is still empty.
  if (title && title.value === "Untitled" && !live) {
    title.value = "";
    title.required = false;
  }
  let title0 = title ? title.value : "";
  // This form sends itself (web/app.js leaves it alone).
  if (form) form.setAttribute("data-js", "");

  // The form sent with fetch: the server saves and answers with a redirect
  // (to this page, or the post's), which is not followed.
  async function saveHere(action) {
    const data = new URLSearchParams(new FormData(form));
    data.set("action", action);
    try {
      const res = await fetch(form.getAttribute("action"), { method: "POST", body: data, credentials: "same-origin", redirect: "manual" });
      if (res.type === "opaqueredirect" || res.ok) {
        edited = false;
        if (title && title.value === "Untitled" && !live) title.value = "";
        title0 = title ? title.value : "";
        saveLabel = action === "publish" ? "Updated · live now" : "Draft saved";
        show(saveLabel, "");
      } else if (res.status === 400) {
        show("Not saved: check the title", "off");
      } else {
        show(res.status === 403 ? "Not saved: are you logged out?" : res.status === 429 ? "Not saved: too many saves in a minute; try again shortly" : "Not saved (" + res.status + ")", "off");
      }
    } catch (e) {
      show("Offline: your text is kept on this device; save again when back online", "off");
    }
  }
  let leaving = false;
  if (form) {
    form.addEventListener("submit", async (ev) => {
      if (form.dataset.flushed === "1") return;
      ev.preventDefault();
      if (title && !title.value.trim()) title.value = "Untitled";
      const by = ev.submitter || null;
      for (const b of form.elements) if (b.tagName === "BUTTON") b.disabled = true;
      show("Saving…", "busy");
      // Until everything is sent, or a request fails (a large paste takes
      // several requests).
      for (let i = 0; i < 1000 && (busy || batches.length); i++) {
        if (busy) await new Promise((r) => setTimeout(r, 50));
        else if (!(await sync())) break;
      }
      const action = by ? by.value : "save";
      // Saving a draft, or updating a post already published, happens
      // here, without leaving the page (the caret and the scroll stay).
      // A first publish goes on to the post.
      if (action === "save" || (action === "publish" && live)) {
        await saveHere(action);
        for (const b of form.elements) if (b.tagName === "BUTTON") b.disabled = false;
        return;
      }
      for (const b of form.elements) if (b.tagName === "BUTTON") b.disabled = false;
      form.dataset.flushed = "1";
      leaving = true;
      setTimeout(() => form.requestSubmit(by), 0);
    });
    // Ctrl+S / Cmd+S: save without leaving the keyboard.
    document.addEventListener("keydown", (ev) => {
      if ((ev.ctrlKey || ev.metaKey) && !ev.altKey && ev.key.toLowerCase() === "s") {
        ev.preventDefault();
        form.requestSubmit();
      }
    });
    // The title is one line that wraps: Enter goes to the text, and pasted
    // line breaks become spaces (the server refuses control characters).
    if (title) {
      title.addEventListener("keydown", (ev) => {
        if (ev.key === "Enter") {
          ev.preventDefault();
          if (modes.visual()) return modes.focus();
          view.focus();
          view.select(0, 0);
        }
      });
      title.addEventListener("input", () => {
        if (/[\r\n]/.test(title.value)) title.value = title.value.replace(/[\r\n]+/g, " ");
        growTitle();
      });
    }
  }

  // Unsent text, or a title not saved yet: ask before leaving.
  window.addEventListener("beforeunload", (ev) => {
    if (leaving) return;
    if (batches.length || (title && title.value !== title0)) ev.preventDefault();
  });

  // The title grows with its text (browsers without CSS field-sizing).
  function growTitle() {
    if (!title || (window.CSS && CSS.supports && CSS.supports("field-sizing", "content"))) return;
    title.style.height = "auto";
    title.style.height = title.scrollHeight + "px";
  }
  growTitle();

  // CHANGES
  // The selection before an edit the browser makes (for undo), and the
  // input method's composition it is part of (one undo step each).
  let selBefore = null;
  let composition = 0, compositions = 0;
  ta.addEventListener("compositionstart", () => (composition = ++compositions));
  ta.addEventListener("compositionend", () => setTimeout(() => (composition = 0), 0));
  ta.addEventListener("beforeinput", (ev) => {
    if (ev.inputType === "historyUndo" || ev.inputType === "historyRedo") {
      ev.preventDefault();
      if (ev.inputType === "historyUndo") undo();
      else redo();
      return;
    }
    const s = view.sel();
    selBefore = { a: s.a, b: s.b };
  });
  ta.addEventListener("keydown", (ev) => {
    const mod = (ev.ctrlKey || ev.metaKey) && !ev.altKey;
    if (!mod || ev.defaultPrevented) return;
    const k = ev.key.toLowerCase();
    if (k === "z" && !ev.shiftKey) {
      ev.preventDefault();
      undo();
    } else if ((k === "z" && ev.shiftKey) || (k === "y" && ev.ctrlKey)) {
      ev.preventDefault();
      redo();
    }
  });

  // The textarea changed the text (typing, paste, drop, IME).
  function typed(p, del, ins, delText) {
    const s = view.sel();
    local(p, del, ins);
    history.record(p, delText, ins, selBefore || { a: p, b: p + del }, { a: s.a, b: s.b }, Date.now(), composition);
    selBefore = null;
  }

  // A change made here other than by the textarea (Vim, Visual mode,
  // undo): into the view, the CRDT and the history; then the selection
  // [a, b] (null: where the view keeps it).
  function change(p, del, ins, a = null, b = a) {
    if (view.readOnly || (del === 0 && ins === "")) return;
    const s = view.sel();
    const delText = view.text.slice(p, p + del);
    view.apply(p, del, ins, false);
    local(p, del, ins);
    if (a !== null) view.select(a, b, true);
    const s2 = view.sel();
    history.record(p, delText, ins, { a: s.a, b: s.b }, { a: s2.a, b: s2.b }, null);
  }

  // The operations for a local change (a long insert in pieces, each
  // whole characters: never a lone half of a surrogate pair).
  function local(p, del, ins) {
    if (ins.length <= PIECE) queue(doc.edit(rep, p, del, ins));
    else {
      let at = 0, first = true;
      while (at < ins.length) {
        let end = Math.min(ins.length, at + PIECE);
        const c = ins.charCodeAt(end - 1);
        if (end < ins.length && c >= 0xd800 && c <= 0xdbff) end--;
        queue(doc.edit(rep, p + at, first ? del : 0, ins.slice(at, end)));
        first = false;
        at = end;
      }
    }
    edited = true;
    saveLabel = "Saved";
    show("Unsaved changes", "busy");
    store();
    schedule(DEBOUNCE_MS);
  }

  function undo() {
    if (view.readOnly) return;
    const ch = history.undo((p, n) => view.text.slice(p, p + n));
    if (!ch) return show("Nothing to undo", batches.length ? "busy" : "");
    applyHistory(ch);
  }

  function redo() {
    if (view.readOnly) return;
    const ch = history.redo((p, n) => view.text.slice(p, p + n));
    if (!ch) return show("Nothing to redo", batches.length ? "busy" : "");
    applyHistory(ch);
  }

  function applyHistory(ch) {
    view.apply(ch.p, ch.del, ch.ins, false);
    local(ch.p, ch.del, ch.ins);
    const s = ch.sel || { a: ch.p + ch.ins.length, b: ch.p + ch.ins.length };
    modes.remote(view.text);
    if (modes.visual()) return;
    view.focus();
    view.select(s.a, s.b, true);
  }

  // Markdown or Visual mode, and Vim keybindings (modes.js). set(text): an
  // edit made there, as the whole text it should become.
  const modes = setupModes(view, {
    set(next, a = null, b = a) {
      if (next === view.text || view.readOnly) return;
      const d = diff(view.text, next);
      change(d.p, d.del, d.ins, a, b);
    },
    save() {
      if (form) form.requestSubmit();
    },
    undo,
    redo,
    // Images are not in this version.
    upload: async () => null,
    show,
  });

  // cls: "" all saved, "busy" work pending, "off" not reaching the server.
  function show(msg, cls) {
    if (!status) return;
    status.textContent = msg;
    status.className = "status" + (cls ? " " + cls : "");
  }

  // REMOTE CHANGES: merged, and each visible change passed to the view (and
  // the undo history) as it happens. A large batch: the text is compared.
  function remote(ops) {
    const got = doc.apply(ops, (p, del, ins) => {
      view.apply(p, del, ins, true);
      history.remote(p, del, ins.length);
    });
    if (got === 2) showDoc();
    if (got) {
      fast = Date.now();
      modes.remote(view.text);
    }
  }

  // The view to the CRDT's text (loading, or after a large batch).
  function showDoc() {
    const next = doc.text();
    if (next === view.text) return;
    const d = diff(view.text, next);
    history.remote(d.p, d.del, d.ins.length);
    view.reset(next);
    modes.remote(next);
  }

  // SYNC
  function queue(ops) {
    if (ops.length) batches.push({ rep, ops });
  }

  function schedule(ms) {
    clearTimeout(timer);
    timer = setTimeout(sync, ms);
  }

  function next() {
    if (batches.length) return DEBOUNCE_MS;
    return Date.now() - fast < 10000 ? FAST_MS : SYNC_MS;
  }

  async function sync() {
    if (busy) return true;
    busy = true;
    let ok = true, more = false;
    // Whole batches of one replica number, oldest first, up to SEND_BYTES.
    let n = 0, size = 0;
    const r = batches.length ? batches[0].rep : rep;
    while (n < batches.length && batches[n].rep === r && (n === 0 || size + batches[n].ops.length <= SEND_BYTES)) size += batches[n++].ops.length;
    const body = new Uint8Array(12 + size);
    const dv = new DataView(body.buffer);
    dv.setBigUint64(0, BigInt(since), true);
    dv.setUint32(8, r, true);
    for (let i = 0, at = 12; i < n; i++) {
      body.set(batches[i].ops, at);
      at += batches[i].ops.length;
    }
    if (n) show("Saving…", "busy");
    try {
      const res = await fetch(url, { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body, credentials: "same-origin" });
      if (!res.ok) {
        show(res.status === 403 ? "Not saved: are you logged out, or no longer an author here?"
          : res.status === 409 ? "Not saved: this post has reached its limit of stored edits. Your text is kept here; copy it into a new post."
          : "Could not save (" + res.status + ")", "off");
        ok = false;
        return false;
      }
      const reply = new Uint8Array(await res.arrayBuffer());
      const rv = new DataView(reply.buffer);
      const kind = reply[0];
      const seq = Number(rv.getBigUint64(1, true));
      more = reply[9] === 1;
      let at = 10;
      if (kind === 1) {
        const len = rv.getUint32(at, true);
        doc.load(reply.subarray(at + 4, at + 4 + len));
        at += 4 + len;
      }
      batches = batches.slice(n);
      const ops = reply.subarray(at);
      if (loaded) {
        if (ops.length) remote(ops);
      } else if (ops.length) doc.apply(ops);
      since = Math.max(since, seq);
      if (!more && !loaded) {
        // The whole document is here: unsent edits from last time go in
        // (a repeat of one already stored changes nothing), then the view.
        for (const b of batches) doc.apply(b.ops);
        loaded = true;
        showDoc();
        view.readOnly = false;
        modes.loaded();
      }
      store();
      show(batches.length ? "Unsaved changes" : savedText(), batches.length ? "busy" : "");
    } catch (e) {
      show("Offline: changes are kept on this device", "off");
      ok = false;
      return false;
    } finally {
      busy = false;
      if (more || (ok && batches.length && n)) schedule(0);
      else schedule(ok ? next() : SYNC_MS);
    }
    return true;
  }

  // Only unsent operations are stored: the server has everything else.
  // Encoding them costs time in proportion to them (a large paste not yet
  // sent is megabytes), so not on every keystroke: at most every STORE_MS,
  // and at once when the page is hidden.
  let storeTimer = 0;
  function store(now = false) {
    if (!now) {
      if (!storeTimer) storeTimer = setTimeout(() => store(true), STORE_MS);
      return;
    }
    clearTimeout(storeTimer);
    storeTimer = 0;
    try {
      if (batches.length) localStorage.setItem(key, JSON.stringify(batches.map((b) => [b.rep, b64(b.ops)])));
      else localStorage.removeItem(key);
    } catch (e) {
      // Storage full or disabled: the server still has everything sent.
    }
  }
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState === "hidden" && storeTimer) store(true);
  });
  window.addEventListener("pagehide", () => {
    if (storeTimer) store(true);
  });

  // Unsent edits from last time (maybe another page load, with its own
  // replica number: sent as that one, which the server gave this writer).
  function restore() {
    try {
      const saved = JSON.parse(localStorage.getItem(key) || "null");
      if (Array.isArray(saved)) for (const [r, s] of saved) if (Number.isInteger(r) && typeof s === "string") batches.push({ rep: r, ops: unb64(s) });
    } catch (e) {
      batches = [];
    }
  }

  function b64(u8) {
    let s = "";
    for (let i = 0; i < u8.length; i += 0x8000) s += String.fromCharCode.apply(null, u8.subarray(i, i + 0x8000));
    return btoa(s);
  }

  function unb64(s) {
    const t = atob(s);
    const u = new Uint8Array(t.length);
    for (let i = 0; i < t.length; i++) u[i] = t.charCodeAt(i);
    return u;
  }

  // START: the WebAssembly, then the document.
  show("Loading…", "busy");
  ready(ta.dataset.wasm).then(() => {
    doc = new Doc();
    window.addEventListener("online", () => schedule(0));
    schedule(0);
  }, () => show("This browser could not load the editor. Your text is safe on the server.", "off"));
}
