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
import { Doc, ready, memoryBytes } from "./crdt.js";
import { View, diff } from "./view.js";
import { History } from "./history.js";
import { setupModes, altOf } from "./modes.js";

// Others' changes are pushed: one request waits at the server until there
// are some (?wait=1, answered within 25 s) and is sent again at once
// (long polling, through any proxy). Only while that fails does the
// editor poll, every SYNC_MS.
const SYNC_MS = 1500;
// A local edit is sent this soon after it (while typing: every SEND_MS,
// not only in pauses, so co-authors see the text as it is typed).
const SEND_MS = 100;
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
  let listening = false;    // a request waits at the server for others' changes
  let pushed = false;       // ...and the last one was answered
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
  ta.ed = { view, doc: () => doc, history, loaded: () => loaded, pending: () => batches.length, memory: memoryBytes };

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
        if (res.status === 403) show("Not saved: you are logged out.", "off", true);
        else show(res.status === 429 ? "Not saved: too many saves in a minute; try again shortly" : "Not saved (" + res.status + ")", "off");
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

  // PREVIEW: the post as it will publish, in a tab of its own. The tab is
  // opened at once (a browser allows a new tab only during the click),
  // then pointed at the preview once unsent edits have reached the server;
  // the title as typed goes with it.
  const previewLink = document.getElementById("preview");
  if (previewLink) {
    previewLink.addEventListener("click", async (ev) => {
      if (ev.ctrlKey || ev.metaKey || ev.shiftKey || ev.button !== 0) return;
      ev.preventDefault();
      const tab = window.open("", "preview");
      for (let i = 0; i < 1000 && (busy || batches.length); i++) {
        if (busy) await new Promise((r) => setTimeout(r, 50));
        else if (!(await sync())) break;
      }
      const url = previewLink.getAttribute("href") + (title && title.value.trim() ? "?title=" + encodeURIComponent(title.value.trim()) : "");
      if (tab) tab.location.href = url;
      else location.href = url;
    });
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
  // typing: typed (merged with the typing before it into one undo step).
  function change(p, del, ins, a = null, b = a, typing = false) {
    if (view.readOnly || (del === 0 && ins === "")) return;
    const s = view.sel();
    const delText = view.text.slice(p, p + del);
    view.apply(p, del, ins, false);
    local(p, del, ins);
    if (a !== null) view.select(a, b, true);
    const s2 = view.sel();
    history.record(p, delText, ins, { a: s.a, b: s.b }, { a: s2.a, b: s2.b }, typing ? Date.now() : null);
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
    soon(SEND_MS);
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
      if (d.del === 0 && d.ins === "") return;
      change(d.p, d.del, d.ins, a, b);
    },
    edit(p, del, ins, a = null, b = a, typing = false) {
      if (view.readOnly || (del === 0 && ins === "")) return;
      change(p, del, ins, a, b, typing);
    },
    save() {
      if (form) form.requestSubmit();
    },
    undo,
    redo,
    upload: uploadImage,
    show,
  });

  // IMAGES: chosen, pasted or dropped. Resized here (at most 1600 px,
  // JPEG) to fit the server's 2 MiB; re-encoding also drops EXIF data such
  // as GPS positions. A small GIF is sent as it is.
  const pick = document.getElementById("image-pick");
  const addBtn = document.getElementById("image-add");
  if (addBtn && pick) {
    addBtn.addEventListener("click", () => pick.click());
    pick.addEventListener("change", () => {
      for (const f of pick.files) upload(f);
      pick.value = "";
    });
  }
  const images = (list) => [...(list || [])].filter((f) => f.type.startsWith("image/"));
  ta.addEventListener("paste", (ev) => {
    const files = images(ev.clipboardData && ev.clipboardData.files);
    if (files.length) {
      ev.preventDefault();
      for (const f of files) upload(f);
    }
  });
  ta.addEventListener("dragover", (ev) => ev.preventDefault());
  ta.addEventListener("drop", (ev) => {
    const files = images(ev.dataTransfer && ev.dataTransfer.files);
    if (files.length) {
      ev.preventDefault();
      for (const f of files) upload(f);
    }
  });

  const IMG_MAX_BYTES = 1900000;
  const IMG_MAX_SIDE = 1600;

  async function shrink(file) {
    if (file.type === "image/gif" && file.size <= IMG_MAX_BYTES) return file;
    const bmp = await createImageBitmap(file, { imageOrientation: "from-image" });
    let scale = Math.min(1, IMG_MAX_SIDE / Math.max(bmp.width, bmp.height));
    for (let round = 0; round < 6; round++) {
      const c = document.createElement("canvas");
      c.width = Math.max(1, Math.round(bmp.width * scale));
      c.height = Math.max(1, Math.round(bmp.height * scale));
      const g = c.getContext("2d");
      g.fillStyle = "#fff";
      g.fillRect(0, 0, c.width, c.height);
      g.drawImage(bmp, 0, 0, c.width, c.height);
      for (const q of [0.85, 0.72, 0.6]) {
        const blob = await new Promise((r) => c.toBlob(r, "image/jpeg", q));
        if (blob && blob.size <= IMG_MAX_BYTES) return blob;
      }
      scale *= 0.7;
    }
    throw new Error("too large");
  }

  // Uploads an image: its path, or null (the status says why).
  async function uploadImage(file) {
    show("Adding the image…", "busy");
    try {
      const blob = await shrink(file);
      const res = await fetch("/upload/" + post, {
        method: "POST",
        headers: { "Content-Type": blob.type || "application/octet-stream" },
        body: blob,
        credentials: "same-origin",
      });
      const text = (await res.text()).trim();
      if (!res.ok || !/^\/img\/[0-9a-f]{32}$/.test(text)) {
        show(res.status === 429 ? "Not added: too many changes in a minute" : "The image was not added", "off");
        return null;
      }
      show(batches.length ? "Unsaved changes" : savedText(), batches.length ? "busy" : "");
      return text;
    } catch (e) {
      show("The image could not be added", "off");
      return null;
    }
  }

  // Into the Markdown text, at the caret (in Visual mode, at its caret).
  async function upload(file) {
    if (modes.visual()) return modes.images([file]);
    const path = await uploadImage(file);
    if (!path || view.readOnly) return;
    const md = "\n![" + altOf(file.name) + "](" + path + ")\n";
    const s = view.sel();
    change(s.a, s.b - s.a, md, s.a + md.length);
  }

  // cls: "" all saved, "busy" work pending, "off" not reaching the server.
  // login: the message offers to log in (a session that ended).
  function show(msg, cls, login = false) {
    if (!status) return;
    status.textContent = msg;
    if (login) {
      const a = document.createElement("a");
      a.href = "/login";
      a.textContent = "Log in";
      status.append(" ", a);
    }
    status.className = "status" + (cls ? " " + cls : "");
  }
  // Logged in again from here (web/app.js's dialog): the unsent text goes
  // now, the page stays.
  document.addEventListener("slop:login", (ev) => {
    ev.preventDefault();
    show("Saving…", "busy");
    schedule(0);
    if (loaded) listen();
  });

  // REMOTE CHANGES: merged, and each visible change passed to the view (and
  // the undo history) as it happens. A large batch: the text is compared.
  function remote(ops) {
    const got = doc.apply(ops, (p, del, ins) => {
      view.apply(p, del, ins, true);
      history.remote(p, del, ins.length);
    });
    if (got === 2) showDoc();
    if (got) modes.remote(view.text);
  }

  // The view to the CRDT's text (loading, or after a large batch).
  function showDoc() {
    const next = doc.text();
    const d = diff(view.text, next);
    if (d.del === 0 && d.ins === "") return;
    history.remote(d.p, d.del, d.ins.length);
    view.reset(next);
    modes.remote(next);
  }

  // SYNC
  function queue(ops) {
    if (ops.length) batches.push({ rep, ops });
  }

  let timerAt = 0;
  function schedule(ms) {
    clearTimeout(timer);
    timerAt = Date.now() + ms;
    timer = setTimeout(() => {
      timerAt = 0;
      sync();
    }, ms);
  }

  // Within ms: a sooner send already planned stands (not pushed back by
  // every key).
  function soon(ms) {
    if (!timerAt || timerAt > Date.now() + ms) schedule(ms);
  }

  // When to send again (null: not until there is something to send, as
  // others' changes are pushed).
  function next() {
    if (batches.length) return SEND_MS;
    return pushed ? null : SYNC_MS;
  }

  function request(r, ops, n, size) {
    const body = new Uint8Array(12 + size);
    const dv = new DataView(body.buffer);
    dv.setBigUint64(0, BigInt(since), true);
    dv.setUint32(8, r, true);
    for (let i = 0, at = 12; i < n; i++) {
      body.set(ops[i].ops, at);
      at += ops[i].ops.length;
    }
    return body;
  }

  // A sync answer: a snapshot (first load), then batches; since moves on.
  // Answers whether more waits at the server.
  function take(reply) {
    const rv = new DataView(reply.buffer, reply.byteOffset, reply.byteLength);
    const kind = reply[0];
    const seq = Number(rv.getBigUint64(1, true));
    const more = reply[9] === 1;
    let at = 10;
    if (kind === 1) {
      const len = rv.getUint32(at, true);
      doc.load(reply.subarray(at + 4, at + 4 + len));
      at += 4 + len;
    }
    const ops = reply.subarray(at);
    if (loaded) {
      if (ops.length) remote(ops);
    } else if (ops.length) doc.apply(ops);
    since = Math.max(since, seq);
    return more;
  }

  // PUSH: a request that waits for others' changes, again and again. Its
  // answers may cross sync()'s: both only move since forward, and an
  // operation applied twice changes nothing.
  async function listen() {
    if (listening) return;
    listening = true;
    pushed = true; // (until it fails: then polling)
    while (loaded && !view.readOnly) {
      const t0 = Date.now();
      let ok = false, got = false;
      try {
        const res = await fetch(url + "?wait=1&me=" + rep, { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body: request(rep, [], 0, 0), credentials: "same-origin" });
        if (res.ok) {
          const reply = new Uint8Array(await res.arrayBuffer());
          got = reply.length > 10;
          ok = true;
          if (take(reply)) got = true;
        } else if (res.status === 403) {
          pushed = false;
          schedule(0); // (logged out: sync() says so; logging in starts this again)
          break;
        }
      } catch (e) {
        // offline: polling says so, and "online" starts this again
      }
      const was = pushed;
      pushed = ok;
      if (!ok) {
        schedule(0); // (polling, until this works again)
        if (!navigator.onLine) break;
        await new Promise((r) => setTimeout(r, SYNC_MS * 2));
      } else if (!got && Date.now() - t0 < 1000) {
        // Answered at once with nothing: the server has too many waiting.
        pushed = false;
        if (was) schedule(SYNC_MS);
        await new Promise((r) => setTimeout(r, SYNC_MS));
      }
    }
    listening = false;
  }

  async function sync() {
    if (busy) return true;
    busy = true;
    let ok = true, more = false;
    // Whole batches of one replica number, oldest first, up to SEND_BYTES.
    let n = 0, size = 0;
    const r = batches.length ? batches[0].rep : rep;
    while (n < batches.length && batches[n].rep === r && (n === 0 || size + batches[n].ops.length <= SEND_BYTES)) size += batches[n++].ops.length;
    const body = request(r, batches, n, size);
    if (n) show("Saving…", "busy");
    try {
      // (&load=1 until the document is here: only then may the answer be a
      // snapshot, which replaces the document.)
      const res = await fetch(url + "?me=" + rep + (loaded ? "" : "&load=1"), { method: "POST", headers: { "Content-Type": "application/octet-stream" }, body, credentials: "same-origin" });
      if (!res.ok) {
        if (res.status === 403) show("Not saved: you are logged out, or no longer an author here. Your text is kept on this device.", "off", true);
        else show(res.status === 409 ? "Not saved: this post has reached its limit of stored edits. Your text is kept here; copy it into a new post."
          : "Could not save (" + res.status + ")", "off");
        ok = false;
        return false;
      }
      const reply = new Uint8Array(await res.arrayBuffer());
      batches = batches.slice(n);
      more = take(reply);
      if (!more && !loaded) {
        // The whole document is here: unsent edits from last time go in
        // (a repeat of one already stored changes nothing), then the view.
        for (const b of batches) doc.apply(b.ops);
        loaded = true;
        showDoc();
        view.readOnly = false;
        modes.loaded();
        listen();
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
      else if (!ok) schedule(SYNC_MS);
      else if (next() !== null) schedule(next());
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
    window.addEventListener("online", () => {
      schedule(0);
      if (loaded) listen();
    });
    schedule(0);
  }, () => show("This browser could not load the editor. Your text is safe on the server.", "off"));
}
