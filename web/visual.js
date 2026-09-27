// The editor's Visual (WYSIWYG) mode. The Markdown text stays the document
// (the textarea, bound to the CRDT); this mode shows it rendered and turns
// edits back into Markdown:
//
// - The text is cut into blocks (runs of lines between blank lines; a
//   fenced code block is one block). Each block is rendered by the proved
//   renderer (src/markdown.rs with src/html.rs, compiled to WebAssembly: web/crdt.js), so what
//   you see is what the server will publish, and the HTML put on the page
//   is provably allowed markup (spec/markup.rs). Pasted HTML is never
//   inserted: pastes are plain text.
// - An edit re-serializes only the blocks it touched (to the Markdown
//   subset the renderer reads); the other blocks keep their exact source,
//   so an edit is a small, local change to the text, as typing is.
// - Remote changes re-render only the blocks whose source changed.
//
// Tested in real Chrome (tests/browser_test.py: render -> serialize ->
// render gives the same HTML for random documents; typing and the toolbar
// change the text as expected).
import { render as renderMd } from "./crdt.js";
import { diff } from "./view.js";

// A block past this is shown read-only: 10x the longest paragraph we can
// imagine (DESIGN.md: 150 KB); the WebAssembly renderer takes ~50 ms for it.
const BLOCK_MAX = 1500000;
// Past this, Visual mode is refused (the post is edited as Markdown): above
// the longest novel (~6M characters, measured within budget:
// tests/large_test.py 6000000), below the 10x case, where each key's
// work on the whole text (a JS string of that length) would be too slow.
export const VISUAL_MAX = 8000000;
// Blocks are rendered when they come this near the screen (the renderer
// takes ~3 ms a KB: a novel at once would take seconds); until then each
// shows its Markdown as plain text, not editable, and far-off ones are
// skipped by the browser (content-visibility). The first and last EAGER
// blocks are rendered at once (Ctrl+Home and Ctrl+End land in them).
const AHEAD_PX = 2000;
const EAGER = 8;

// The text as prefix + blocks, each {md, sep}: text = prefix + md0 + sep0
// + md1 + sep1 ... A block is a run of non-blank lines, or a fenced code
// block (blank lines and all); sep is what follows it up to the next block
// (line breaks and blank lines). Exact by construction: every part is a
// slice of the text, and the slices tile it.
export function split(text) {
  const lines = [];
  for (let s = 0; ; ) {
    const e = text.indexOf("\n", s);
    if (e < 0) {
      lines.push({ s, e: text.length });
      break;
    }
    lines.push({ s, e });
    s = e + 1;
  }
  const n = lines.length;
  const line = (k) => text.slice(lines[k].s, lines[k].e);
  const blank = (k) => line(k).trim() === "";
  const fence = (k) => /^```/.test(line(k));
  let i = 0;
  while (i < n && blank(i)) i++;
  const prefix = text.slice(0, i < n ? lines[i].s : text.length);
  const blocks = [];
  while (i < n) {
    const start = i;
    if (fence(i)) {
      i++;
      while (i < n && !fence(i)) i++;
      if (i < n) i++;
    } else {
      while (i < n && !blank(i) && !(i > start && fence(i))) i++;
    }
    const s0 = lines[start].s, e0 = lines[i - 1].e;
    while (i < n && blank(i)) i++;
    const next = i < n ? lines[i].s : text.length;
    blocks.push({ md: text.slice(s0, e0), sep: text.slice(e0, next) });
  }
  return { prefix, blocks };
}

function render(md) {
  if (md.length > BLOCK_MAX) return null;
  try {
    return renderMd(md);
  } catch (e) {
    return null;
  }
}

// MARKDOWN FROM THE EDITED HTML: the subset src/markdown.rs reads.
const esc = (s) => s.replace(/[\\*_`[\]]/g, "\\$&");
// A paragraph line that would start a block is escaped.
const escStart = (s) => s.replace(/^(\s*)([#>+-]|\d+\.|```)/, "$1\\$2");

function inline(node) {
  let out = "";
  for (const n of node.childNodes) {
    if (n.nodeType === 3) {
      out += esc(n.nodeValue.replace(/ /g, " "));
      continue;
    }
    if (n.nodeType !== 1) continue;
    const tag = n.tagName;
    if (tag === "BR") out += "\n";
    else if (tag === "STRONG" || tag === "B") out += wrap("**", inline(n));
    else if (tag === "EM" || tag === "I") out += wrap("*", inline(n));
    else if (tag === "CODE") {
      const c = n.textContent.replace(/`/g, "'").replace(/\n/g, " ");
      out += c ? "`" + c + "`" : "";
    } else if (tag === "A") {
      const href = n.getAttribute("href") || "";
      const text = inline(n);
      out += /^(https?:\/\/|\/)[^\s()<>"]*$/.test(href) && text ? "[" + text + "](" + href + ")" : text;
    } else if (tag === "IMG") {
      const src = n.getAttribute("src") || "";
      if (/^\/img\/[0-9a-f]{32}$/.test(src)) out += "![" + (n.getAttribute("alt") || "").replace(/[[\]\n]/g, " ") + "](" + src + ")";
    } else out += inline(n);
  }
  return out;
}

// Emphasis around text: the markers go inside any spaces at the ends.
function wrap(m, s) {
  const lead = s.match(/^\s*/)[0], trail = s.match(/\s*$/)[0];
  const core = s.slice(lead.length, s.length - trail.length);
  return core ? lead + m + core + m + trail : s;
}

const para = (s) =>
  s.split("\n").map((l) => escStart(l.trim())).filter((l) => l !== "").join("\n");

function blocksOf(el) {
  const out = [];
  let run = null; // inline content directly in el, gathered into a paragraph
  const flush = () => {
    if (run !== null) {
      const p = para(run);
      if (p) out.push(p);
      run = null;
    }
  };
  for (const n of el.childNodes) {
    const tag = n.nodeType === 1 ? n.tagName : "";
    if (n.nodeType === 3 || ["STRONG", "B", "EM", "I", "CODE", "A", "IMG", "SPAN", "BR", "FONT", "U", "S"].includes(tag)) {
      const frag = document.createElement("span");
      frag.appendChild(n.cloneNode(true));
      run = (run || "") + inline(frag);
      continue;
    }
    flush();
    if (n.nodeType !== 1) continue;
    if (tag === "P" || tag === "DIV") out.push(...blocksOf(n));
    else if (tag === "H1" || tag === "H2") {
      const s = inline(n).replace(/\n/g, " ").trim();
      if (s) out.push("## " + s);
    } else if (/^H[3-6]$/.test(tag)) {
      const s = inline(n).replace(/\n/g, " ").trim();
      if (s) out.push("### " + s);
    } else if (tag === "UL" || tag === "OL") {
      const items = [];
      listItems(n, 0, items);
      if (items.length) out.push(items.join("\n"));
    } else if (tag === "BLOCKQUOTE") {
      const inner = blocksOf(n).join("\n\n");
      if (inner) out.push(inner.split("\n").map((l) => "> " + l).join("\n"));
    } else if (tag === "PRE") {
      out.push("```\n" + n.innerText.replace(/\n$/, "").replace(/^```/gm, "'''") + "\n```");
    } else if (tag === "HR") {
      continue;
    } else out.push(...blocksOf(n));
  }
  flush();
  return out;
}

// A block element's Markdown.
export function serialize(el) {
  return blocksOf(el).join("\n\n");
}

export class Visual {
  // text: the Markdown; onChange(text): an edit made here.
  constructor(root, onChange) {
    this.root = root;
    this.onChange = onChange;
    this.prefix = "";
    this.last = null; // the text's parts as last reported (see commit)
    this.stray = false; // something was added straight into the root
    this.dirty = new Set();
    this.observer = new MutationObserver((recs) => this.collect(recs));
    // Blocks are drawn as they come near the screen, found from scroll
    // events: at most once a frame, walking out from the block on screen.
    // (An IntersectionObserver on every block cost 18 ms a key in a novel:
    // the browser checks all of them after every change.)
    this.queued = false;
    this.opened = false;
    this.onScroll = () => {
      if (this.queued || !this.opened) return;
      this.queued = true;
      requestAnimationFrame(() => {
        this.queued = false;
        this.drawNear();
      });
    };
    // Before the browser types into an emptied view, there is a paragraph.
    root.addEventListener("beforeinput", () => this.ensure());
    // Find in page reached a block not drawn yet: draw it.
    root.addEventListener("beforematch", (ev) => {
      const b = blockOf(root, ev.target);
      if (b) this.draw(b);
    });
    root.addEventListener("input", (ev) => {
      // (Typing and deleting are merged into one undo step, as in the
      // Markdown text; formatting is a step of its own.)
      if (!this.shortcut(ev)) this.commit(/^(insertText|insertCompositionText|deleteContent)/.test(ev.inputType || ""));
    });
    // Pasting: formatting that Markdown has survives (bold, italic, links,
    // headings, lists, quotes, code), safely: the pasted HTML is parsed
    // into an inert document (nothing in it runs or loads), written as
    // Markdown by serialize, and rendered by the proved renderer, so only
    // allowed markup reaches the page. Images are the editor's (uploaded).
    root.addEventListener("paste", (ev) => {
      const cd = ev.clipboardData;
      const files = [...(cd ? cd.files : [])].filter((f) => f.type.startsWith("image/"));
      if (files.length) return; // images: the editor uploads them (see editor.js)
      ev.preventDefault();
      const html = cd ? cd.getData("text/html") : "";
      const t = cd ? cd.getData("text/plain") : "";
      if (html) {
        const doc = new DOMParser().parseFromString(html, "text/html");
        docsStyles(doc.body);
        const md = serialize(doc.body);
        const out = md ? render(md) : null;
        if (out) {
          // One paragraph: its content only (no new block around it).
          const one = /^<p>([\s\S]*)<\/p>\n?$/.exec(out);
          document.execCommand("insertHTML", false, one && !one[1].includes("<p>") ? one[1] : out);
          return;
        }
      }
      if (t) document.execCommand("insertText", false, t);
    });  }

  // The blocks mutations touched (and blocks added at the top level).
  collect(recs) {
    for (const r of recs) {
      if (r.target === this.root)
        for (const a of r.addedNodes) {
          this.dirty.add(a);
          // (Content typed straight into the root: gather() has work.)
          if (a.nodeType !== 1 || !a.classList.contains("wblock")) this.stray = true;
        }
      const b = blockOf(this.root, r.target);
      if (b) this.dirty.add(b);
    }
  }

  open(text) {
    document.execCommand("defaultParagraphSeparator", false, "p");
    this.opened = true;
    this.show(split(text));
    this.observer.observe(this.root, { childList: true, subtree: true, characterData: true });
    window.addEventListener("scroll", this.onScroll, { passive: true });
    window.addEventListener("resize", this.onScroll, { passive: true });
  }

  close() {
    this.opened = false;
    this.last = null;
    this.observer.disconnect();
    window.removeEventListener("scroll", this.onScroll);
    window.removeEventListener("resize", this.onScroll);
    this.root.replaceChildren();
  }

  // A block, not rendered yet (see AHEAD_PX).
  block(b) {
    const el = document.createElement("div");
    el.className = "wblock wlazy";
    el.contentEditable = "false";
    // (Skipped by the browser without watching it; found by find in page.)
    el.setAttribute("hidden", "until-found");
    el.textContent = b.md;
    el.md = b.md;
    el.sep = b.sep;
    return el;
  }

  // Renders a block (its Markdown is kept: rendering is not an edit).
  draw(el) {
    if (!el.classList.contains("wlazy")) return;
    el.removeAttribute("hidden");
    const others = this.observer.takeRecords();
    const html = render(el.md);
    el.classList.remove("wlazy");
    if (html === null) {
      // Too long for this mode: shown as text, edited in Markdown mode.
      el.classList.add("wraw");
      el.title = "A long block: edit it in Markdown mode";
    } else {
      el.innerHTML = html; // allowed markup only (spec/markup.rs)
      el.removeAttribute("contenteditable");
    }
    this.observer.takeRecords();
    this.collect(others);
  }

  show({ prefix, blocks }) {
    this.prefix = prefix;
    this.tail = blocks.length ? blocks[blocks.length - 1].sep : "";
    const els = blocks.map((b) => this.block(b));
    // An empty document: one empty paragraph to type in.
    if (!els.length || (els.length === 1 && !blocks[0].md)) {
      const el = document.createElement("div");
      el.className = "wblock";
      el.innerHTML = "<p><br></p>";
      el.md = "";
      el.sep = "";
      els.splice(0, els.length, el);
    }
    this.root.replaceChildren(...els);
    for (let i = 0; i < els.length; i++) if (i < EAGER || i >= els.length - EAGER) this.draw(els[i]);
    this.drawNear();
    this.observer.takeRecords();
    this.dirty.clear();
    this.last = this.parts();
  }

  // Draws the blocks within AHEAD_PX of the screen: from the block on screen
  // (or the first or last, when the view is above or below it) outwards,
  // until a block is that far away.
  drawNear() {
    const r = this.root.getBoundingClientRect();
    const h = window.innerHeight;
    // (Not shown yet, as when opening: nothing is near; asked again once it is.)
    if (r.height === 0) return this.onScroll();
    if (!this.root.firstElementChild || r.bottom < -AHEAD_PX || r.top > h + AHEAD_PX) return;
    let mid = null;
    if (r.top > 0) mid = this.root.firstElementChild;
    else if (r.bottom < h) mid = this.root.lastElementChild;
    else {
      const x = Math.min(Math.max(r.left + 10, 0), window.innerWidth - 1);
      const hit = document.elementFromPoint(x, h / 2);
      mid = hit && this.root.contains(hit) ? blockOf(this.root, hit) : null;
      if (!mid) {
        // (Between blocks: a quick search by position.)
        const kids = this.root.children;
        let lo = 0, hi = kids.length - 1;
        while (lo < hi) {
          const m = (lo + hi) >> 1;
          if (kids[m].getBoundingClientRect().bottom < h / 2) lo = m + 1;
          else hi = m;
        }
        mid = kids[lo];
      }
    }
    for (let el = mid; el; el = el.nextElementSibling) {
      if (el.getBoundingClientRect().top > h + AHEAD_PX) break;
      this.draw(el);
    }
    for (let el = mid && mid.previousElementSibling; el; el = el.previousElementSibling) {
      if (el.getBoundingClientRect().bottom < -AHEAD_PX) break;
      this.draw(el);
    }
  }

  // Markdown shortcuts: "## ", "### ", "- ", "1. " or "> " typed at the
  // start of a paragraph make it a heading, a list or a quote. Answers
  // whether it did (the edits it makes are committed as they happen).
  shortcut(ev) {
    if (ev.inputType !== "insertText" || ev.data !== " ") return false;
    const sel = document.getSelection();
    if (!sel || !sel.isCollapsed || !this.root.contains(sel.anchorNode)) return false;
    let block = sel.anchorNode.nodeType === 1 ? sel.anchorNode : sel.anchorNode.parentNode;
    while (block && block !== this.root && !/^(P|DIV)$/.test(block.tagName)) block = block.parentNode;
    if (!block || block === this.root || block.classList.contains("wblock") && block.querySelector("p,h2,h3,ul,ol,blockquote,pre")) return false;
    const before = offsetIn(block, sel.anchorNode, sel.anchorOffset);
    const m = block.textContent.slice(0, before).match(/^(#{1,3}|[-*+]|1[.)]|>)[ \u00a0]$/);
    if (!m) return false;
    const r = document.createRange();
    r.setStart(block, 0);
    r.setEnd(sel.anchorNode, sel.anchorOffset);
    sel.removeAllRanges();
    sel.addRange(r);
    document.execCommand("delete");
    const k = m[1];
    if (k === "###") document.execCommand("formatBlock", false, "h3");
    else if (k[0] === "#") document.execCommand("formatBlock", false, "h2");
    else if (k === ">") {
      document.execCommand("formatBlock", false, "blockquote");
      quoteParagraphs(this.root);
    }
    else if (k[0] === "1") document.execCommand("insertOrderedList");
    else document.execCommand("insertUnorderedList");
    this.commit();
    return true;
  }

  // A change the mutation observer does not see (an attribute: a link's
  // address): its block is written again at the next commit.
  touch(node) {
    const b = blockOf(this.root, node);
    if (b) this.dirty.add(b);
  }

  // An edit: the touched blocks become Markdown again, and the change is
  // reported as it is ({p, del, ins}): the parts of the text before and
  // after are compared part by part (unchanged blocks are the same
  // strings), and only the stretch that differs is compared character by
  // character. A novel is tens of thousands of blocks; the whole text is
  // never built for an edit.
  commit(typing = false) {
    this.collect(this.observer.takeRecords());
    this.gather();
    this.observer.takeRecords();
    for (const el of this.dirty) {
      if (el.parentNode !== this.root || el.classList.contains("wraw") || el.classList.contains("wlazy")) continue;
      el.md = serialize(el);
    }
    this.dirty.clear();
    const prev = this.last, next = this.parts();
    this.last = next;
    if (!prev) return this.onChange(next.join(""), typing);
    let a = 0;
    while (a < prev.length && a < next.length && prev[a] === next[a]) a++;
    if (a === prev.length && a === next.length) return;
    let z = 0;
    while (z < prev.length - a && z < next.length - a && prev[prev.length - 1 - z] === next[next.length - 1 - z]) z++;
    let p = 0;
    for (let i = 0; i < a; i++) p += prev[i].length;
    const d = diff(prev.slice(a, prev.length - z).join(""), next.slice(a, next.length - z).join(""));
    if (d.del === 0 && d.ins === "") return;
    this.onChange({ p: p + d.p, del: d.del, ins: d.ins }, typing);
  }

  // Content typed straight into the root (Safari does, once everything was
  // deleted): each run of it gathered into one paragraph block, and the
  // caret put back where it was (moving a node takes a selection out of
  // it: typing then went on in the root, a letter per block).
  gather() {
    // (Only when something landed in the root: walking every block on every
    // key costs milliseconds in a novel.)
    if (!this.stray) return this.ensure();
    this.stray = false;
    const sel = document.getSelection();
    const at = sel && sel.rangeCount ? { node: sel.anchorNode, off: sel.anchorOffset } : null;
    let moved = false;
    let run = null;
    for (const n of [...this.root.childNodes]) {
      if (n.nodeType === 1 && n.classList.contains("wblock")) {
        run = null;
        continue;
      }
      if (!run) {
        run = document.createElement("div");
        run.className = "wblock";
        run.appendChild(document.createElement("p"));
        this.root.insertBefore(run, n);
        this.dirty.add(run);
      }
      run.firstChild.appendChild(n);
      moved = moved || (at && (at.node === n || n.contains(at.node)));
    }
    if (moved) sel.collapse(at.node, at.off);
    this.ensure();
  }

  // Never empty: an empty paragraph to type in (and the caret in it, if the
  // caret was left in the root).
  ensure() {
    if (this.root.querySelector(".wblock")) return;
    const el = document.createElement("div");
    el.className = "wblock";
    el.innerHTML = "<p><br></p>";
    el.md = "";
    el.sep = "";
    this.root.appendChild(el);
    const sel = document.getSelection();
    if (sel && this.root.contains(sel.anchorNode)) sel.collapse(el.firstChild, 0);
  }

  // The text as the blocks now say: empty blocks leave no text (the page
  // keeps them: the caret may be there), a block followed by another is
  // separated by a blank line whatever it was before, and the last one is
  // followed by what ended the text (whichever block is last now: one
  // joined into the one before takes its place).
  text() {
    return this.parts().join("");
  }

  // The text in parts, in order: the prefix, then each block's Markdown
  // and what follows it.
  parts() {
    const out = [this.prefix];
    let prev = null;
    for (let el = this.root.firstElementChild; el; el = el.nextElementSibling) {
      if (!el.md) continue;
      if (prev) out.push(prev.md, between(prev));
      prev = el;
    }
    if (prev) out.push(prev.md, this.tail);
    return out;
  }

  // The text changed elsewhere (another writer): re-render what differs.
  refresh(text) {
    if (text === this.text()) return;
    const next = split(text);
    const olds = [...this.root.children];
    let a = 0;
    while (a < olds.length && a < next.blocks.length && olds[a].md === next.blocks[a].md) a++;
    let z = 0;
    while (z < olds.length - a && z < next.blocks.length - a && olds[olds.length - 1 - z].md === next.blocks[next.blocks.length - 1 - z].md) z++;
    const sel = document.getSelection();
    const focusBlock = sel && sel.anchorNode && this.root.contains(sel.anchorNode) ? blockOf(this.root, sel.anchorNode) : null;
    const focusAt = focusBlock ? offsetIn(focusBlock, sel.anchorNode, sel.anchorOffset) : 0;
    const focusIndex = focusBlock ? olds.indexOf(focusBlock) : -1;
    const fresh = next.blocks.slice(a, next.blocks.length - z).map((b) => this.block(b));
    const after = olds[olds.length - z] || null;
    for (const el of olds.slice(a, olds.length - z)) {
      el.remove();
    }
    for (const el of fresh) this.root.insertBefore(el, after);
    // Changed blocks on screen are drawn now, not a frame later.
    for (const el of fresh) if (el.getBoundingClientRect().top < window.innerHeight + 2000) this.draw(el);
    this.prefix = next.prefix;
    this.tail = next.blocks.length ? next.blocks[next.blocks.length - 1].sep : "";
    [...this.root.children].forEach((el, i) => (el.sep = next.blocks[i].sep));
    if (focusIndex >= a && focusIndex < olds.length - z && fresh.length) {
      const el = fresh[Math.min(focusIndex - a, fresh.length - 1)];
      placeCaret(el, focusAt);
    }
    this.dirty.clear();
    this.observer.takeRecords();
    this.last = this.parts();
  }
}

// What separates a block from the next: its own separator if that has a
// blank line, else a blank line (cached on the element: the same answer
// until its separator changes).
function between(el) {
  const sep = el.sep ?? "";
  if (el._sepFor !== sep) {
    el._sepFor = sep;
    el._sepIs = /\n[ \t]*\n/.test(sep) ? sep : "\n\n";
  }
  return el._sepIs;
}

// A list's items as Markdown lines, nested lists indented to their
// parent item's text (CommonMark: a sub-list starts at the column of the
// item's content). Both shapes of nesting: a list inside an item (as the
// renderer makes it) and a list right inside a list (as the browser's
// indent command makes it: it belongs to the item before it).
function listItems(list, indent, out) {
  const ordered = list.tagName === "OL";
  let k = 0, width = ordered ? 3 : 2;
  for (const c of list.children) {
    if (c.tagName === "LI") {
      const own = c.cloneNode(true);
      for (const sub of own.querySelectorAll(":scope > ul, :scope > ol")) sub.remove();
      const s = inline(own).replace(/\n/g, " ").trim();
      const marker = ordered ? ++k + ". " : "- ";
      width = marker.length;
      if (s) out.push(" ".repeat(indent) + marker + s);
      for (const sub of c.querySelectorAll(":scope > ul, :scope > ol")) listItems(sub, indent + width, out);
    } else if (c.tagName === "UL" || c.tagName === "OL") {
      listItems(c, indent + width, out);
    }
  }
}

// Pasted HTML made ready to write as Markdown. Formatting written as
// styles (Google Docs, Word): a <b> or <strong>
// that says it is not bold is unwrapped (Docs wraps a whole paste in one),
// and spans with bold or italic styles become <b> and <i>.
function docsStyles(root) {
  // (What is not text on the page: scripts, style sheets and the like.)
  for (const x of [...root.querySelectorAll("script, style, template, noscript, meta, link, title, svg, math, iframe, object")]) x.remove();
  for (const b of [...root.querySelectorAll("b, strong")]) {
    if (/font-weight:\s*(normal|[1-5]00)\b/.test(b.getAttribute("style") || "")) b.replaceWith(...b.childNodes);
  }
  for (const sp of [...root.querySelectorAll("span[style]")]) {
    const st = sp.getAttribute("style");
    let n = sp;
    if (/font-style:\s*italic/.test(st)) {
      const i = document.createElement("i");
      i.append(...n.childNodes);
      n.append(i);
    }
    if (/font-weight:\s*(bold|[6-9]00)\b/.test(st)) {
      const b = document.createElement("b");
      b.append(...n.childNodes);
      n.append(b);
    }
  }
}

// Quotes hold paragraphs (the browser's quote command puts text straight
// in the quote, and Enter then starts a second quote): each run of text
// and inline elements in a quote is wrapped in a paragraph, the caret kept.
export function quoteParagraphs(root) {
  const sel = document.getSelection();
  const keep = sel.rangeCount ? [sel.anchorNode, sel.anchorOffset, sel.focusNode, sel.focusOffset] : null;
  for (const q of root.querySelectorAll("blockquote")) {
    let run = null;
    for (const n of [...q.childNodes]) {
      const block = n.nodeType === 1 && /^(P|DIV|UL|OL|PRE|BLOCKQUOTE|H[1-6])$/.test(n.tagName);
      if (block) {
        run = null;
        continue;
      }
      if (!run) {
        run = document.createElement("p");
        q.insertBefore(run, n);
      }
      run.appendChild(n);
    }
  }
  if (keep) sel.setBaseAndExtent(keep[0], keep[1], keep[2], keep[3]);
}

function blockOf(root, n) {
  while (n && n.parentNode !== root) n = n.parentNode;
  return n;
}

function offsetIn(el, node, off) {
  const r = document.createRange();
  r.setStart(el, 0);
  try {
    r.setEnd(node, off);
  } catch (e) {
    return 0;
  }
  return r.toString().length;
}

function placeCaret(el, at) {
  const w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
  let n, left = at;
  while ((n = w.nextNode())) {
    if (left <= n.nodeValue.length) {
      document.getSelection().collapse(n, left);
      return;
    }
    left -= n.nodeValue.length;
  }
  document.getSelection().collapse(el, el.childNodes.length);
}
