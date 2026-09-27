// The editor's modes: Markdown (the textarea) or Visual (WYSIWYG,
// visual.js), and, in Markdown mode, Vim keybindings (vim.js): off by
// default; the first time they are turned on, a dialog explains them and
// asks the writer to say yes only if they know Vim. Choices are kept per
// browser (localStorage: a convenience, not a setting that must follow
// the writer).
//
// Built here, not in the page template: without JavaScript the editor is
// a plain textarea and none of this applies.
import { Vim } from "./vim.js";
import { Visual, VISUAL_MAX, quoteParagraphs } from "./visual.js";

const PREF_MODE = "slop:editor-mode";
const PREF_VIM = "slop:vim";
const PREF_VIM_OK = "slop:vim-ok";

function pref(k) {
  try {
    return localStorage.getItem(k);
  } catch (e) {
    return null;
  }
}
function setPref(k, v) {
  try {
    if (v === null) localStorage.removeItem(k);
    else localStorage.setItem(k, v);
  } catch (e) {
    // Storage off: the choice lasts for this page only.
  }
}

function el(tag, attrs = {}, ...kids) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") e.className = v;
    else if (k === "text") e.textContent = v;
    else e.setAttribute(k, v);
  }
  e.append(...kids);
  return e;
}

// view: the Markdown view (web/view.js); set(text, a, b): replace the
// text as an edit (CRDT, history and status follow), then select [a, b];
// edit(p, del, ins): the same for a change already known (preferred: no
// comparison of whole texts, which are megabytes in a novel);
// save(): save now; undo(), redo(): the editor's; upload(file) ->
// "/img/..." or null.
export function setupModes(view, { set, edit, save, undo, redo, upload, show }) {
  const ta = view.ta;
  const md = el("button", { type: "button", class: "seg", "aria-pressed": "true", text: "Markdown" });
  const vis = el("button", { type: "button", class: "seg", "aria-pressed": "false", text: "Visual" });
  const B = (label, title, cmd) => el("button", { type: "button", class: "fmt", title, "aria-label": title, "data-cmd": cmd, text: label });
  const fmt = el("div", { class: "fmt-bar", role: "toolbar", "aria-label": "Formatting", hidden: "" },
    B("B", "Bold (Ctrl+B)", "bold"), B("I", "Italic (Ctrl+I)", "italic"), B("</>", "Code", "code"), B("Link", "Link (Ctrl+K)", "link"),
    B("H2", "Heading", "h2"), B("H3", "Subheading", "h3"), B("¶", "Paragraph", "p"), B("❝", "Quote", "quote"),
    B("• List", "Bulleted list", "ul"), B("1. List", "Numbered list", "ol"), B("{ }", "Code block", "pre"), B("Image", "Add an image", "image"));
  const vimBtn = el("button", { type: "button", class: "seg vim-toggle", "aria-pressed": "false", title: "Vim keybindings", text: "Vim" });
  const modeLine = el("span", { class: "vim-line", role: "status", "aria-live": "polite", hidden: "" });
  // Into the page's empty toolbar (its height is kept for it: no shift).
  let bar = document.getElementById("edit-tools");
  if (!bar) {
    bar = el("div", { class: "edit-tools" });
    view.box.parentNode.insertBefore(bar, view.box);
  }
  bar.append(el("div", { class: "segs", role: "group", "aria-label": "Editing mode" }, md, vis), fmt,
    el("div", { class: "tools-end" }, modeLine, vimBtn));
  const wys = el("div", { class: "body wys", contenteditable: "true", role: "textbox", "aria-multiline": "true", "aria-label": "Text", hidden: "", spellcheck: "true" });
  view.box.parentNode.insertBefore(wys, view.box.nextSibling);
  const dialog = vimDialog();
  document.body.appendChild(dialog);

  // Visual mode reports a change ({p, del, ins}) or, rarely, the whole text.
  const visual = new Visual(wys, (ch, typing) => (typeof ch === "string" ? set(ch) : edit(ch.p, ch.del, ch.ins, null, null, typing)));
  let mode = "markdown";

  // MODES
  // Visual mode needs the document and the renderer (WebAssembly): asked
  // for before they arrive (the saved preference, or a click while
  // loading), it opens when they have.
  let visualWanted = false;
  function toMode(m, focus = true) {
    if (m === "visual" && view.readOnly) {
      visualWanted = true;
      vis.setAttribute("aria-pressed", "true");
      md.setAttribute("aria-pressed", "false");
      return;
    }
    if (m === "markdown") visualWanted = false;
    if (m === "visual" && view.length > VISUAL_MAX) {
      show("This post is too long for Visual mode: edit it as Markdown", "off");
      m = "markdown";
    }
    if (m === mode) return;
    mode = m;
    document.body.classList.toggle("visual-mode", m === "visual");
    setPref(PREF_MODE, m);
    md.setAttribute("aria-pressed", String(m === "markdown"));
    vis.setAttribute("aria-pressed", String(m === "visual"));
    fmt.hidden = m !== "visual";
    vimBtn.hidden = m === "visual"; // Vim is for the Markdown text
    if (m === "visual") {
      if (vimOn) vimShow(false);
      visual.open(view.text);
      view.box.hidden = true;
      wys.hidden = false;
      if (focus) wys.focus();
    } else {
      visual.close();
      wys.hidden = true;
      view.box.hidden = false;
      if (vimOn) vimShow(true);
      if (focus) view.focus();
    }
  }
  md.addEventListener("click", () => toMode("markdown"));
  vis.addEventListener("click", () => toMode("visual"));

  // FORMATTING (Visual mode). Each button turns its format on, or off
  // where it already is (a heading back to a paragraph, a quote or code
  // unwrapped); the toolbar shows which apply where the caret is.
  // The element of tag (a list of tags) around the caret, inside the text.
  function around(tags) {
    const sel = document.getSelection();
    let n = sel && sel.rangeCount ? sel.anchorNode : null;
    for (; n && n !== wys; n = n.parentNode) if (n.nodeType === 1 && tags.includes(n.tagName)) return n;
    return null;
  }
  // Replaces el with its children, keeping the caret or selection where it
  // was (moving nodes out would otherwise collapse it), then commits.
  function unwrap(el) {
    const sel = document.getSelection();
    const keep = sel.rangeCount ? [sel.anchorNode, sel.anchorOffset, sel.focusNode, sel.focusOffset] : null;
    const parent = el.parentNode;
    while (el.firstChild) parent.insertBefore(el.firstChild, el);
    el.remove();
    if (keep) sel.setBaseAndExtent(keep[0], keep[1], keep[2], keep[3]);
    visual.commit();
  }
  function format(cmd) {
    wys.focus();
    if (cmd === "bold" || cmd === "italic") document.execCommand(cmd);
    else if (cmd === "h2" || cmd === "h3") {
      const h = around(["H1", "H2", "H3", "H4", "H5", "H6"]);
      const same = h && (cmd === "h3" ? /^H[3-6]$/.test(h.tagName) : /^H[12]$/.test(h.tagName));
      document.execCommand("formatBlock", false, same ? "p" : cmd);
    } else if (cmd === "p") document.execCommand("formatBlock", false, "p");
    else if (cmd === "pre") {
      const pre = around(["PRE"]);
      // (A rendered code block has <code> inside: taken out with the block,
      // else the text would stay code, inline.)
      if (pre) for (const c of [...pre.querySelectorAll("code")]) c.replaceWith(...c.childNodes);
      document.execCommand("formatBlock", false, pre ? "p" : "pre");
    }
    else if (cmd === "quote") {
      const q = around(["BLOCKQUOTE"]);
      if (q) unwrap(q);
      else {
        document.execCommand("formatBlock", false, "blockquote");
        quoteParagraphs(wys);
        visual.commit();
      }
    } else if (cmd === "ul") document.execCommand("insertUnorderedList");
    else if (cmd === "ol") document.execCommand("insertOrderedList");
    else if (cmd === "code") {
      const c = around(["CODE"]);
      if (c && c.parentNode.tagName !== "PRE") unwrap(c);
      else {
        const s = document.getSelection().toString().replace(/\n/g, " ");
        if (s) document.execCommand("insertHTML", false, "<code>" + escHtml(s) + "</code>");
      }
    } else if (cmd === "link") linkDialog();
    else if (cmd === "image") imgPick.click();
    state();
  }

  // Which formats apply at the caret: shown as pressed buttons.
  function state() {
    if (mode !== "visual") return;
    const on = {
      bold: !!around(["B", "STRONG"]) || (!around(["H1", "H2", "H3", "H4", "H5", "H6"]) && document.queryCommandState("bold")),
      italic: !!around(["I", "EM"]),
      code: !!around(["CODE"]),
      link: !!around(["A"]),
      h2: !!around(["H1", "H2"]),
      h3: !!around(["H3", "H4", "H5", "H6"]),
      quote: !!around(["BLOCKQUOTE"]),
      pre: !!around(["PRE"]),
      ul: (around(["UL", "OL"]) || {}).tagName === "UL",
      ol: (around(["UL", "OL"]) || {}).tagName === "OL",
    };
    for (const b of fmt.querySelectorAll("button[data-cmd]")) {
      if (b.dataset.cmd in on) b.setAttribute("aria-pressed", String(on[b.dataset.cmd]));
    }
  }
  document.addEventListener("selectionchange", () => {
    if (mode === "visual" && wys.contains(document.getSelection().anchorNode)) state();
  });

  // LINKS: a dialog (the page's, not the browser's prompt): add a link to
  // the selected text (or the address as its text), change one, remove it.
  const linkDlg = el("dialog", { class: "modal", "aria-labelledby": "link-title" });
  const linkUrl = el("input", { type: "url", name: "url", required: "", placeholder: "https://…", autocomplete: "off", "aria-describedby": "link-note" });
  const linkNote = el("p", { class: "meta", id: "link-note", text: "An address starting with https://, http:// or mailto:, or a page here starting with /." });
  const linkRemove = el("button", { type: "button", class: "quiet", text: "Remove link" });
  linkDlg.append(el("form", { method: "dialog", class: "stack" },
    el("h2", { id: "link-title", text: "Link" }),
    el("label", {}, "Link to ", linkUrl), linkNote,
    el("div", { class: "row" }, linkRemove, el("button", { value: "cancel", class: "quiet", formnovalidate: "", text: "Cancel" }), el("button", { value: "ok", class: "primary", text: "Save link" }))));
  let linkRange = null, linkEl = null;
  const LINK_OK = /^(https?:\/\/[^\s<>"]+|mailto:[^\s<>"]+|\/[^\s<>"]*)$/;
  function linkDialog() {
    const sel = document.getSelection();
    if (!sel.rangeCount || !wys.contains(sel.anchorNode)) return;
    linkRange = sel.getRangeAt(0).cloneRange();
    linkEl = around(["A"]);
    linkUrl.value = linkEl ? linkEl.getAttribute("href") : "https://";
    linkRemove.hidden = !linkEl;
    linkNote.classList.remove("bad");
    if (!linkDlg.isConnected) document.body.appendChild(linkDlg);
    linkDlg.showModal();
    linkUrl.select();
  }
  linkDlg.addEventListener("submit", (ev) => {
    if (ev.submitter && ev.submitter.value !== "ok") return;
    const url = linkUrl.value.trim();
    if (!LINK_OK.test(url)) {
      ev.preventDefault();
      linkNote.classList.add("bad");
      linkUrl.focus();
    }
  });
  linkDlg.addEventListener("close", () => {
    wys.focus();
    const sel = document.getSelection();
    if (linkRange) {
      sel.removeAllRanges();
      sel.addRange(linkRange);
    }
    if (linkDlg.returnValue !== "ok") return state();
    const url = linkUrl.value.trim();
    if (linkEl) {
      linkEl.setAttribute("href", url);
      visual.touch(linkEl);
      visual.commit();
    } else if (sel.isCollapsed) document.execCommand("insertHTML", false, '<a href="' + escHtml(url) + '">' + escHtml(url) + "</a>");
    else document.execCommand("createLink", false, url);
    state();
  });
  linkRemove.addEventListener("click", () => {
    const a = linkEl;
    linkDlg.close("cancel");
    if (a) unwrap(a);
    state();
  });
  fmt.addEventListener("click", (ev) => {
    const b = ev.target.closest("button[data-cmd]");
    if (b) format(b.dataset.cmd);
  });
  fmt.addEventListener("mousedown", (ev) => ev.preventDefault()); // keep the selection in the text
  wys.addEventListener("keydown", (ev) => {
    if (!(ev.ctrlKey || ev.metaKey) || ev.altKey) return;
    const k = ev.key.toLowerCase();
    if (k === "z" || (k === "y" && ev.ctrlKey)) {
      // The editor's undo (of the text), not the page's (of the markup).
      ev.preventDefault();
      if (k === "z" && !ev.shiftKey) undo();
      else redo();
    } else if (k === "b" || k === "i") {
      ev.preventDefault();
      format(k === "b" ? "bold" : "italic");
    } else if (k === "k") {
      ev.preventDefault();
      format("link");
    }
  });
  // Images into Visual mode: uploaded, then put in at the caret.
  async function addImages(files, ev) {
    if (!files.length) return;
    if (ev) ev.preventDefault();
    // Where the caret was: the upload takes a while, and the caret may move.
    const sel = document.getSelection();
    const at = sel.rangeCount && wys.contains(sel.anchorNode) ? sel.getRangeAt(0).cloneRange() : null;
    for (const f of files) {
      const path = await upload(f);
      if (path) {
        wys.focus();
        if (at) {
          sel.removeAllRanges();
          sel.addRange(at);
        }
        document.execCommand("insertHTML", false, '<img src="' + path + '" alt="' + escHtml(altOf(f.name)) + '">');
        if (at) at.collapse(false);
      }
    }
  }
  // The toolbar's Image button: a file picker of its own.
  const imgPick = el("input", { type: "file", accept: "image/*", hidden: "", multiple: "" });
  fmt.append(imgPick);
  imgPick.addEventListener("change", () => {
    addImages([...imgPick.files], null);
    imgPick.value = "";
  });
  // ENTER AND BACKSPACE where the browser's own does the unexpected:
  // - Enter on an empty line in a quote leaves the quote (as in a list);
  // - Backspace at the very start of a list item, a quote's first
  //   paragraph or a heading first takes that formatting away (the
  //   browser would join it to the block before).
  function atStart(block) {
    const sel = document.getSelection();
    if (!sel.isCollapsed || !block.contains(sel.anchorNode)) return false;
    const r = document.createRange();
    r.setStart(block, 0);
    r.setEnd(sel.anchorNode, sel.anchorOffset);
    return r.toString() === "" && !r.cloneContents().querySelector("img");
  }
  wys.addEventListener("keydown", (ev) => {
    if (ev.ctrlKey || ev.metaKey || ev.altKey || ev.isComposing) return;
    if (ev.key === "Enter" && !ev.shiftKey) {
      const q = around(["BLOCKQUOTE"]);
      const p = around(["P", "DIV"]);
      if (q && p && q.contains(p) && p !== q && p.textContent === "" && !p.querySelector("img")) {
        ev.preventDefault();
        const out = document.createElement("p");
        out.appendChild(document.createElement("br"));
        q.after(out);
        p.remove();
        if (!q.textContent && !q.querySelector("img")) q.remove();
        document.getSelection().collapse(out, 0);
        visual.commit();
        state();
      }
      return;
    }
    if (ev.key !== "Backspace" || ev.shiftKey) return;
    const li = around(["LI"]);
    if (li && atStart(li)) {
      ev.preventDefault();
      document.execCommand("outdent");
      state();
      return;
    }
    const q = around(["BLOCKQUOTE"]);
    if (q) {
      const first = q.firstElementChild || q;
      if (atStart(first) && first.contains(document.getSelection().anchorNode)) {
        ev.preventDefault();
        if (first === q) unwrap(q);
        else {
          const sel = document.getSelection();
          const keep = [sel.anchorNode, sel.anchorOffset];
          q.before(first);
          if (!q.firstChild || (!q.textContent && !q.querySelector("img"))) q.remove();
          sel.collapse(keep[0], keep[1]);
          visual.commit();
        }
        state();
        return;
      }
    }
    const h = around(["H1", "H2", "H3", "H4", "H5", "H6"]);
    if (h && atStart(h)) {
      ev.preventDefault();
      document.execCommand("formatBlock", false, "p");
      state();
    }
  });

  // Tab in Visual mode: inside a list, the item nests (Shift+Tab: back
  // out); elsewhere Tab moves on, as on any page.
  wys.addEventListener("keydown", (ev) => {
    if (ev.key !== "Tab" || ev.ctrlKey || ev.metaKey || ev.altKey) return;
    const sel = document.getSelection();
    const n = sel && sel.anchorNode;
    const li = n && (n.nodeType === 1 ? n : n.parentNode).closest("li");
    if (!li || !wys.contains(li)) return;
    ev.preventDefault();
    document.execCommand(ev.shiftKey ? "outdent" : "indent");
  });
  wys.addEventListener("beforeinput", (ev) => {
    if (ev.inputType === "historyUndo" || ev.inputType === "historyRedo") {
      ev.preventDefault();
      if (ev.inputType === "historyUndo") undo();
      else redo();
    }
  });
  wys.addEventListener("paste", (ev) => addImages([...(ev.clipboardData ? ev.clipboardData.files : [])].filter((f) => f.type.startsWith("image/")), ev));
  wys.addEventListener("drop", (ev) => addImages([...(ev.dataTransfer ? ev.dataTransfer.files : [])].filter((f) => f.type.startsWith("image/")), ev));

  // VIM
  const vim = new Vim();
  let vimOn = false;
  function vimShow(on) {
    modeLine.hidden = !on;
    ta.classList.toggle("vim", on);
    if (on) {
      if (vim.mode === "insert") vim.escape(view.text, view.sel().a);
      vim.mode = "normal";
      block(view.sel().a);
      line();
    }
  }
  function vimSet(on) {
    vimOn = on;
    setPref(PREF_VIM, on ? "1" : null);
    vimBtn.setAttribute("aria-pressed", String(on));
    vimShow(on && mode === "markdown");
    if (mode === "markdown") view.focus();
  }
  vimBtn.addEventListener("click", () => {
    if (vimOn) return vimSet(false);
    if (pref(PREF_VIM_OK) === "1") return vimSet(true);
    dialog.returnValue = "";
    dialog.showModal();
  });
  dialog.addEventListener("close", () => {
    if (dialog.returnValue === "yes") {
      setPref(PREF_VIM_OK, "1");
      vimSet(true);
    } else view.focus();
  });

  // Normal mode stands on a character: shown as a one-character selection.
  function block(c) {
    const t = view.text;
    c = Vim.normalize(t, c);
    const e = c < t.length && t[c] !== "\n" ? c + (t.codePointAt(c) > 0xffff ? 2 : 1) : c;
    view.select(c, e, true);
  }
  function line(extra = "") {
    const names = { normal: "NORMAL", insert: "INSERT", visual: "VISUAL", vline: "VISUAL LINE" };
    modeLine.textContent = vim.cmd !== null ? vim.cmd + vim.cmdText : "-- " + names[vim.mode] + " --" + (extra ? "  " + extra : "");
    modeLine.classList.toggle("insert", vim.mode === "insert");
  }
  const NAMED = new Set(["Escape", "Enter", "Backspace", "ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown", "Home", "End"]);
  ta.addEventListener("keydown", (ev) => {
    if (!vimOn || mode !== "markdown" || ev.isComposing) return;
    let k = ev.key;
    if (ev.ctrlKey && !ev.altKey && !ev.metaKey && (k === "[" || k === "c") && vim.mode === "insert") k = "Escape";
    else if (ev.ctrlKey && !ev.altKey && !ev.metaKey && k.toLowerCase() === "r") k = "C-r";
    else if (ev.ctrlKey || ev.metaKey || ev.altKey) return; // Ctrl+S and the browser's own
    if (vim.mode === "insert" && vim.cmd === null) {
      if (k !== "Escape") return; // typing: the browser's
      ev.preventDefault();
      const r = vim.escape(view.text, view.sel().a);
      block(r.cur);
      line();
      return;
    }
    if ([...k].length !== 1 && !NAMED.has(k) && k !== "C-r") return; // Shift, Tab...
    ev.preventDefault();
    if (view.readOnly) return;
    const s = view.sel();
    const r = vim.key(k, { text: view.text, a: s.a, b: s.b });
    if (!r) return;
    // The change as Vim made it (no whole-text comparison), else found.
    if (r.edit) edit(r.edit.p, r.edit.del, r.edit.ins);
    else if (r.text !== view.text) set(r.text);
    if (r.keepCursor) block(view.sel().a);
    else if (vim.mode === "insert") view.select(r.a, r.a, true);
    else if (vim.mode === "visual" || vim.mode === "vline") view.select(r.a, r.b, true);
    else block(r.a);
    line(r.msg || r.pending || "");
    if (r.save) save();
  });
  // TAB. In Markdown mode: a tab character, or with lines selected (or
  // Shift), each line indented (a tab) or outdented (a tab or up to four
  // spaces). Vim's normal mode leaves Tab alone, as Vim does. To leave the
  // editor by keyboard: Esc, then Tab (the textarea keeps Tab otherwise).
  let escaped = false;
  ta.addEventListener("keydown", (ev) => {
    if (ev.key === "Escape") {
      escaped = !vimOn;
      return;
    }
    if (ev.key !== "Tab" || ev.ctrlKey || ev.metaKey || ev.altKey || ev.defaultPrevented || ev.isComposing) {
      if (ev.key !== "Shift") escaped = false;
      return;
    }
    if (escaped) {
      escaped = false;
      return; // (the browser moves the focus on)
    }
    if (mode !== "markdown" || view.readOnly || (vimOn && vim.mode !== "insert")) return;
    ev.preventDefault();
    tab(ev.shiftKey);
  });

  function tab(out) {
    const t = view.text;
    const s = view.sel();
    if (s.a === s.b && !out) {
      edit(s.a, 0, "\t", s.a + 1, s.a + 1);
      return;
    }
    // The whole lines the selection touches (a selection ending at a
    // line's start does not take that line).
    const ls = t.lastIndexOf("\n", s.a - 1) + 1;
    const last = s.b > s.a && t[s.b - 1] === "\n" ? s.b - 1 : s.b;
    let le = t.indexOf("\n", last);
    if (le < 0) le = t.length;
    const lines = t.slice(ls, le).split("\n");
    let da = 0, db = 0; // how far the selection's ends move
    const next = lines.map((l, i) => {
      let n;
      if (!out) n = l === "" && lines.length > 1 ? l : "\t" + l;
      else {
        const m = l.match(/^(\t| {1,4})/);
        n = m ? l.slice(m[0].length) : l;
      }
      const d = n.length - l.length;
      if (i === 0) da = Math.max(d, ls - s.a);
      db += d;
      return n;
    }).join("\n");
    if (next === t.slice(ls, le)) return;
    const a = Math.max(ls, s.a + da), b = Math.max(a, s.b + db);
    edit(ls, le - ls, next, a, b);
  }

  // A click in normal mode moves the block cursor there.
  ta.addEventListener("mouseup", () => {
    if (!vimOn || mode !== "markdown" || vim.mode === "insert") return;
    const s = view.sel();
    if (s.b - s.a > 2) {
      vim.mode = "visual";
      vim.anchor = s.a;
      vim.head = s.b - 1;
      line();
    } else {
      vim.mode = "normal";
      block(s.a);
      line();
    }
  });

  // Where we were last time.
  if (pref(PREF_VIM) === "1" && pref(PREF_VIM_OK) === "1") vimSet(true);
  if (pref(PREF_MODE) === "visual") toMode("visual", false);

  return {
    // The text changed elsewhere (another writer, a sync, undo).
    remote(text) {
      if (mode === "visual") visual.refresh(text);
    },
    // The document arrived: Visual mode shows it.
    loaded() {
      if (visualWanted && mode !== "visual") {
        visualWanted = false;
        toMode("visual", false);
      } else if (mode === "visual") visual.refresh(view.text);
    },
    visual: () => mode === "visual",
    // Images from elsewhere (the page's own button) go in at Visual mode's caret.
    images: (files) => addImages(files, null),
    focus() {
      if (mode === "visual") wys.focus();
      else view.focus();
    },
  };
}

function vimDialog() {
  const d = el("dialog", { class: "modal", "aria-labelledby": "vim-title" });
  const form = el("form", { method: "dialog", class: "stack" },
    el("h2", { id: "vim-title", text: "Turn on Vim keybindings?" }),
    el("p", { text: "Vim is a modal editor. With this on, the editor starts in Normal mode, where keys are commands, not text: pressing a letter moves the cursor or changes the text instead of typing it." }),
    el("p", { text: "Press i to type, Esc to stop typing, and :w to save. Your text still saves as you type, and you can turn this off again with the Vim button." }),
    el("p", { class: "warn", text: "Only click Yes if you already know Vim. If you are not sure, click No." }),
    el("div", { class: "row" },
      el("button", { value: "no", class: "primary", autofocus: "", text: "No, keep normal typing" }),
      el("button", { value: "yes", class: "quiet", text: "Yes, I know Vim" })));
  d.appendChild(form);
  return d;
}

function escHtml(s) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
}

export function altOf(name) {
  return (name || "").replace(/\.[a-z0-9]+$/i, "").replace(/[\[\]\r\n]/g, " ").slice(0, 100).trim();
}
