#!/usr/bin/env python3
"""Using the editor as a writer does, in real Chrome (keys and the mouse as
a person sends them), checking what they would see:

- writing a post: the title, Enter into the text, paragraphs, Backspace
  across lines, replacing a selection, pasting, undo and redo by words;
- saving with Ctrl+S (no page load; the text and title still there after a
  reload);
- two writers: each one's undo leaves the other's words alone; both see
  the same text in the end;
- offline: typing goes on, the status says so, and it all syncs later;
- an input method (composition): the composed text, and undoing it;
- the window's edges in a long post: Backspace and Enter right at them,
  a selection from the window to far-off static text (Shift+click) deleted,
  a 300 KB paste (the window grows, then shrinks back);
- Visual mode: Ctrl+B, then Ctrl+Z undoes it in the Markdown;
- the page: no layout shift while a long post loads; on a phone-sized
  screen nothing is wider than the screen and typing works; after a resize
  the caret is still on screen and typing goes where it is.

(Ported from the old app's tests/usability_test.py. The server's text is
read by applying the stored snapshot and operations with build/app.wasm,
the server's own CRDT compiled to WebAssembly; a long post is stored the
way a form without JavaScript sends one.)

usage: tests/usability_test.py   (needs tools/build.sh and tools/setup_chrome.sh)
"""
import hashlib, json, os, random, re, secrets, socket, sqlite3, struct, subprocess, sys, tempfile, time, urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import start_chrome, page_ws, wait_port

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8082
BASE = f"http://localhost:{PORT}"
B = "document.getElementById('editor').ed"
NODE = os.environ.get("NODE", os.path.expanduser("~/opt/node/bin/node"))
FRAME = "new Promise(r => requestAnimationFrame(() => setTimeout(() => r(performance.now()), 0)))"
fails = 0
BUDGET_DB = None


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"), flush=True)


def fresh_budget():
    # This test writes far faster than a person (30 writes a minute a user).
    if BUDGET_DB:
        c = sqlite3.connect(BUDGET_DB, timeout=5)
        c.execute("DELETE FROM write_budget")
        c.commit()
        c.close()


def http(method, path, sid=None, form=None):
    if method == "POST":
        fresh_budget()
    body = urllib.parse.urlencode(form).encode() if form is not None else b""
    h = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nSec-Fetch-Site: same-origin\r\n"
    if sid:
        h += f"Cookie: sid={sid}\r\n"
    if method == "POST":
        h += f"Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {len(body)}\r\n"
    s = socket.create_connection(("127.0.0.1", PORT), timeout=60)
    s.sendall(h.encode() + b"\r\n" + body)
    out = b""
    while True:
        b = s.recv(65536)
        if not b:
            break
        out += b
    s.close()
    head, _, rb = out.partition(b"\r\n\r\n")
    loc = re.search(rb"\r\nLocation: ([^\r]*)", head)
    return int(head.split(b" ")[1]), (loc.group(1).decode() if loc else None), rb.decode("utf-8", "replace")


def user(db, email):
    name = email.split("@")[0]
    uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES (?, ?, ?, 1)", (email, name, name)).lastrowid
    raw = secrets.token_bytes(32)
    now = int(time.time() * 1000)
    db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw).digest(), uid, now, now + 3600_000))
    db.commit()
    return uid, raw.hex()


def novel(n, seed=7):
    rng = random.Random(seed)
    words = ("the river of long evenings carries small boats past old walls where people talk about books "
             "and café light falls on streets that remember summers 🌊 nobody wrote down").split()
    out, size, para = [], 0, 0
    while size < n:
        if para % 30 == 0:
            s = f"## Chapter {para // 30 + 1}"
        else:
            s = " ".join(rng.choice(words) for _ in range(rng.randint(30, 220)))
            s = s[0].upper() + s[1:] + "."
        out.append(s)
        size += len(s) + 2
        para += 1
    text = "\n\n".join(out)
    # Cut at a code point, within n UTF-16 units.
    units = 0
    for i, ch in enumerate(text):
        units += 2 if ord(ch) > 0xFFFF else 1
        if units > n:
            return text[:i]
    return text


def u16(s):
    """s with each UTF-16 unit as one character (an emoji as its two
    surrogates), so Python's indexes are the editor's."""
    b = s.encode("utf-16-le", "surrogatepass")
    return "".join(chr(b[i] | b[i + 1] << 8) for i in range(0, len(b), 2))


def store(sid, pid, text):
    """The post's text, as a form sent without JavaScript sets it (it
    replaces the document's)."""
    st, _, _ = http("POST", f"/edit/{pid}", sid, {"title": "Untitled", "body": text})
    assert st == 303, st


# The server's document: its snapshot, then its stored batches, applied by
# the server's CRDT compiled to WebAssembly (the browser's build/app.wasm;
# tests/wasm_test.mjs checks it against the native build).
MATERIALIZE = r"""
import { readFileSync } from "node:fs";
const { instance } = await WebAssembly.instantiate(readFileSync(process.argv[1]), {});
const w = instance.exports;
const put = (b) => { const p = w.input(b.length); new Uint8Array(w.memory.buffer, p, b.length).set(b); };
const out = () => new Uint8Array(w.memory.buffer, w.output(), w.output_len()).slice();
const inp = readFileSync(0);
const dv = new DataView(inp.buffer, inp.byteOffset, inp.byteLength);
w.reset();
let at = 0;
const snap = dv.getUint32(0, true); at = 4;
if (snap) { put(inp.subarray(4, 4 + snap)); if (w.load() < 0) { console.log("BAD SNAPSHOT"); process.exit(1); } at += snap; }
while (at < inp.length) {
  const n = dv.getUint32(at, true);
  put(inp.subarray(at + 4, at + 4 + n));
  if (w.apply() < 0) { console.log("BAD BATCH"); process.exit(1); }
  at += 4 + n;
}
w.text();
process.stdout.write("<<TEXT>>" + new TextDecoder().decode(out()) + "<<END>>");
"""


def server_text(db, pid):
    """The post's text as the server has it (stored, not the page's)."""
    db.commit()
    snap = db.execute("SELECT upto, data FROM doc_snap WHERE post_id = ?", (pid,)).fetchone()
    upto, data = snap if snap else (0, b"")
    buf = struct.pack("<I", len(data)) + data
    for (d,) in db.execute("SELECT data FROM doc_ops WHERE post_id = ? AND seq > ? ORDER BY seq", (pid, upto)):
        buf += struct.pack("<I", len(d)) + d
    r = subprocess.run([NODE, "--input-type=module", "-e", MATERIALIZE, os.path.join(ROOT, "build/app.wasm")],
                       input=buf, capture_output=True)
    outs = r.stdout.decode("utf-8")
    if "<<TEXT>>" not in outs:
        print("  (materialize:", outs[:200], r.stderr.decode()[:300], ")")
        return None
    return u16(outs.split("<<TEXT>>", 1)[1].rsplit("<<END>>", 1)[0])


KEYS = {"Escape": 27, "Enter": 13, "Backspace": 8, "Tab": 9}


class Page:
    def __init__(self, port, profile, sid):
        self.proc = start_chrome(port, profile)
        self.ws = page_ws(port)
        for d in ("Page", "Runtime", "Network"):
            self.ws.call(d + ".enable")
        self.ws.call("Network.setCookie", {"name": "sid", "value": sid, "url": BASE, "httpOnly": True, "sameSite": "Strict"})

    def js(self, expr):
        r = self.ws.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})
        return r.get("result", {}).get("value")

    def open(self, url):
        self.ws.call("Page.navigate", {"url": url})
        for _ in range(60):
            time.sleep(0.1)
            if self.js("document.readyState") == "complete" and self.js("!!document.querySelector('.edit-tools')"):
                break
        time.sleep(0.4)

    def key(self, k, ctrl=False):
        """One key as a keyboard sends it (a named key, or a character)."""
        mods = 2 if ctrl else 0
        if k in KEYS:
            base = {"key": k, "code": k, "windowsVirtualKeyCode": KEYS[k], "modifiers": mods}
            text = {"Enter": "\r"}.get(k)
            self.ws.call("Input.dispatchKeyEvent", dict(base, type="keyDown" if text else "rawKeyDown", **({"text": text} if text else {})))
            self.ws.call("Input.dispatchKeyEvent", dict(base, type="keyUp"))
        else:
            code = ord(k.upper()) if k.isalpha() else 0
            ev = {"key": k, "modifiers": mods, "windowsVirtualKeyCode": code}
            if ctrl:
                self.ws.call("Input.dispatchKeyEvent", dict(ev, type="rawKeyDown"))
            else:
                self.ws.call("Input.dispatchKeyEvent", dict(ev, type="keyDown", text=k))
            self.ws.call("Input.dispatchKeyEvent", dict(ev, type="keyUp"))

    def keys(self, s):
        for tok in re.findall(r"<[A-Za-z-]+>|.", s):
            if tok.startswith("<") and len(tok) > 1:
                name = tok[1:-1]
                if name.startswith("C-"):
                    self.key(name[2:], ctrl=True)
                else:
                    self.key({"Esc": "Escape", "CR": "Enter", "BS": "Backspace"}.get(name, name))
            else:
                self.key(tok)
        time.sleep(0.15)

    def full(self):
        t = self.js(f"{B}.view.text")
        return None if t is None else u16(t)

    def sel(self):
        return self.js(f"(() => {{ const s = {B}.view.sel(); return [s.a, s.b]; }})()")

    def win(self):
        return self.js(f"(() => {{ const w = {B}.view.win(); return [w.s, w.e]; }})()")

    def wait_loaded(self, secs=120):
        t = time.time()
        while time.time() - t < secs:
            if self.js(f"!!document.getElementById('editor') && !!{B} && {B}.loaded()"):
                return time.time() - t
            time.sleep(0.05)
        return None

    def wait_synced(self, secs=120):
        t = time.time()
        while time.time() - t < secs:
            if self.js(f"{B}.pending()") == 0:
                return True
            time.sleep(0.1)
        return False

    def per_key(self, keys):
        """Median and worst time from a key to the next frame, in ms."""
        lat = []
        for k in keys:
            s = self.js("performance.now()")
            self.key(k)
            lat.append(self.js(FRAME) - s)
        lat.sort()
        return lat[len(lat) // 2], lat[-1]

    def caret_y(self):
        """The caret's top on screen (a mirror of the textarea)."""
        return self.js("""(() => {
          const ta = document.getElementById('editor');
          const m = document.createElement('div');
          const cs = getComputedStyle(ta);
          m.style.cssText = 'position:absolute;visibility:hidden;white-space:pre-wrap;overflow-wrap:break-word;top:0;left:0';
          m.style.width = ta.clientWidth + 'px'; m.style.font = cs.font; m.style.padding = cs.padding;
          m.textContent = ta.value.slice(0, ta.selectionEnd);
          const k = document.createElement('span'); k.textContent = '\\u200b'; m.appendChild(k);
          document.body.appendChild(m); const y = k.offsetTop; m.remove();
          return ta.getBoundingClientRect().top + y; })()""")

    def close(self):
        self.proc.terminate()
        self.proc.wait()


def key(br, name, code, vk, mods=0):
    br.ws.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": name, "code": code, "windowsVirtualKeyCode": vk, "modifiers": mods})
    br.ws.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": name, "code": code, "windowsVirtualKeyCode": vk, "modifiers": mods})


CTRL, SHIFT = 2, 8


def ctrl(br, ch, shift=False):
    key(br, ch, "Key" + ch.upper(), ord(ch.upper()), CTRL | (SHIFT if shift else 0))
    time.sleep(0.1)


def status(br):
    return br.js("document.getElementById('sync-status').textContent")


def new_post(a, slug):
    _, loc, _ = http("POST", "/dash/u/posts", a, {"slug": slug, "title": "Untitled"})
    return int(loc.rsplit("/", 1)[1])


def open_post(br, pid):
    # (Unsent edits would hold the page with the leave-this-page prompt.)
    if br.js(f"!!document.getElementById('editor') && !!{B}"):
        br.wait_synced(60)
    br.open(f"{BASE}/edit/{pid}")
    return br.wait_loaded()


def page(port, tmp, a, name, w=1280, h=900, mobile=False):
    br = Page(port, f"{tmp}/{name}", a)
    br.ws.call("Emulation.setDeviceMetricsOverride", {"width": w, "height": h, "deviceScaleFactor": 1, "mobile": mobile})
    if mobile:
        br.ws.call("Emulation.setTouchEmulationEnabled", {"enabled": True})
    br.open(BASE + "/")
    br.js("localStorage.setItem('slop:vim', '0'); localStorage.setItem('slop:editor-mode', 'markdown')")
    return br


def writing(br, db, a):
    pid = new_post(a, "first")
    open_post(br, pid)
    br.js("document.querySelector('textarea.title').focus()")
    br.keys("My first post")
    br.keys("<CR>")
    check("Enter in the title goes to the text", br.js("document.activeElement.id") == "editor" and br.sel() == [0, 0])
    br.keys("Hello there<CR><CR>Second paragraph")
    check("typing paragraphs", br.full() == "Hello there\n\nSecond paragraph", repr(br.full()))
    # Backspace at the start of the second paragraph: joins the lines.
    br.js(f"{B}.view.select(13, 13)")
    br.keys("<BS>")
    check("Backspace at a line's start joins it to the one before", br.full() == "Hello there\nSecond paragraph", repr(br.full()))
    br.keys("<CR>")
    # Shift+Left five times selects "there"; typing replaces it.
    br.js(f"{B}.view.select(11, 11)")
    for _ in range(5):
        key(br, "ArrowLeft", "ArrowLeft", 37, SHIFT)
    br.keys("world")
    check("typing replaces a selection", br.full() == "Hello world\n\nSecond paragraph", repr(br.full()))
    # A paste (as the browser inserts it).
    br.js(f"{B}.view.select({len(br.full())}, {len(br.full())})")
    br.ws.call("Input.insertText", {"text": "\n\n> A quoted line\n\n- one\n- two"})
    time.sleep(0.2)
    after_paste = br.full()
    check("a paste goes in at the caret", after_paste.endswith("paragraph\n\n> A quoted line\n\n- one\n- two"), repr(after_paste))
    check("the status says there are unsaved changes, then saved", "Unsaved" in status(br) or "Saving" in status(br) or "Saved" in status(br))
    time.sleep(2)
    check("…Saved", status(br).startswith("Saved"), status(br))
    # Undo: the paste, then the typing a word at a time; redo.
    time.sleep(1.6)
    ctrl(br, "z")
    check("Ctrl+Z undoes the paste", br.full() == "Hello world\n\nSecond paragraph", repr(br.full()))
    ctrl(br, "z")
    check("…then the replacement", br.full() == "Hello there\n\nSecond paragraph", repr(br.full()))
    ctrl(br, "z", shift=True)
    ctrl(br, "z", shift=True)
    check("Ctrl+Shift+Z redoes both", br.full() == after_paste, repr(br.full()))
    check("…and the caret is after the redone text", br.sel()[0] == len(after_paste), br.sel())
    # Save: stays on the page; after a reload, all there.
    br.js("window.__marker = 1")
    fresh_budget()
    ctrl(br, "s")
    time.sleep(1.5)
    check("Ctrl+S saves without leaving the page", br.js("window.__marker") == 1 and "saved" in status(br).lower(), status(br))
    open_post(br, pid)
    check("after a reload: the text", br.full() == after_paste, repr(br.full()))
    check("…and the title", br.js("document.querySelector('textarea.title').value") == "My first post", br.js("document.querySelector('textarea.title').value"))
    # The rendered post: publish it (the old app rendered on save; here a
    # publish renders the document's Markdown).
    st, _, _ = http("POST", f"/edit/{pid}/publish", a)
    _, _, html = http("GET", "/b/u/first")
    check("the saved post is rendered", st == 303 and "<blockquote>" in html and "<li>one</li>" in html and "My first post" in html, (st, html[:300]))


def together(br, b2, db, a):
    pid = new_post(a, "together")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("Line one.<CR>Line two.")
    time.sleep(2)
    open_post(b2, pid)
    check("the second writer sees the text", b2.full() == "Line one.\nLine two.", repr(b2.full()))
    b2.js("document.getElementById('editor').focus()")
    b2.js(f"{B}.view.select(9, 9)")
    b2.keys(" Theirs")
    time.sleep(2.5)
    check("their words arrive", br.full() == "Line one. Theirs\nLine two.", repr(br.full()))
    br.js(f"{B}.view.select({len(br.full())}, {len(br.full())})")
    time.sleep(1.6)
    br.keys(" Mine")
    time.sleep(0.3)
    ctrl(br, "z")
    check("my undo takes away my last word", br.full() == "Line one. Theirs\nLine two.", repr(br.full()))
    for _ in range(8):
        ctrl(br, "z")
    time.sleep(2.5)
    check("…and undoing all I wrote leaves their words", br.full() == " Theirs", repr(br.full()))
    check("…and the other writer sees the same", b2.full() == br.full(), (b2.full(), br.full()))
    before = br.full()
    # Both typing at the same place at once.
    br.js(f"{B}.view.select(0, 0)")
    b2.js(f"{B}.view.select(0, 0)")
    for c in "abc":
        br.keys(c)
        b2.keys(c.upper())
    time.sleep(3)
    check("typing at the same place at once: both converge", br.full() == b2.full() and len(br.full()) == 6 + len(before), (br.full(), b2.full()))
    check("…to what the server has", br.wait_synced() and server_text(db, pid) == br.full(), (server_text(db, pid), br.full()))


def offline(br, db, a):
    pid = new_post(a, "offline")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("Before. ")
    time.sleep(2)
    br.ws.call("Network.emulateNetworkConditions", {"offline": True, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})
    br.keys("Written offline.")
    time.sleep(3)
    check("offline: the status says the text is kept here", "Offline" in status(br), status(br))
    check("…and typing went on", br.full() == "Before. Written offline.")
    stored = br.js(f"localStorage.getItem('slop:post:{pid}')")
    try:
        kept = json.loads(stored) if stored else None
    except ValueError:
        kept = None
    check("…the unsent edits are kept in the browser", isinstance(kept, list) and len(kept) > 0, stored)
    br.ws.call("Network.emulateNetworkConditions", {"offline": False, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})
    br.js("window.dispatchEvent(new Event('online'))")
    time.sleep(3)
    check("back online: it all syncs", status(br).startswith("Saved") and server_text(db, pid) == "Before. Written offline.", (status(br), server_text(db, pid)))


def ime(br, a):
    pid = new_post(a, "ime")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("Tokyo: ")
    time.sleep(1.6)
    for part in ("と", "とう", "とうき", "とうきょう"):
        br.ws.call("Input.imeSetComposition", {"text": part, "selectionStart": len(part), "selectionEnd": len(part)})
        time.sleep(0.05)
    br.ws.call("Input.insertText", {"text": "東京"})
    time.sleep(0.3)
    check("an input method's composition gives the composed text", br.full() == "Tokyo: 東京", repr(br.full()))
    time.sleep(1.6)
    ctrl(br, "z")
    check("…and undo takes it away", br.full() == "Tokyo: ", repr(br.full()))


def edges(br, db, a):
    pid = new_post(a, "edges")
    text = novel(200_000, seed=3)
    store(a, pid, text)
    text = u16(text)
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.js(f"{B}.view.select(120000, 120000, true)")
    time.sleep(0.4)
    ws, we = br.win()
    check("a 200 KB post: the window is part of it", ws > 0 and we < len(text), (ws, we))
    # Backspace right at the window's start (a line start): joins lines.
    br.js("document.getElementById('editor').setSelectionRange(0, 0)")
    br.keys("<BS>")
    time.sleep(0.2)
    want = text[: ws - 1] + text[ws:]
    check("Backspace at the window's very start joins the line before", br.full() == want, (br.full()[ws - 20: ws + 20] if br.full() else None))
    text = want
    # Enter at the window's end.
    ws, we = br.win()
    br.js("(() => { const t = document.getElementById('editor'); t.setSelectionRange(t.value.length, t.value.length); })()")
    br.keys("<CR>")
    time.sleep(0.2)
    want = text[:we] + "\n" + text[we:]
    check("Enter at the window's very end", br.full() == want, (br.full()[we - 10: we + 10] if br.full() else None))
    text = want
    # Delete at the window's end: the line break after it goes.
    ws, we = br.win()
    br.js("(() => { const t = document.getElementById('editor'); t.setSelectionRange(t.value.length, t.value.length); })()")
    key(br, "Delete", "Delete", 46)
    time.sleep(0.2)
    want = text[:we] + text[we + 1:]
    check("Delete at the window's very end joins the next line", br.full() == want, (br.full()[we - 10: we + 10] if br.full() else None))
    text = want
    # Shift+click far below, in static text: the selection reaches there.
    br.js(f"{B}.view.select(60000, 60000, true)")
    time.sleep(0.4)
    target = text.index("## Chapter", 150000)
    br.js(f"""(() => {{ const v = {B}.view; let s = 0; for (const pc of v.pieces) {{ if (!pc.win && s <= {target} && {target} <= s + pc.len) {{
        const r = document.createRange(); r.setStart(pc.el.firstChild, {target} - s); r.setEnd(pc.el.firstChild, {target} - s + 1);
        window.scrollTo(0, window.scrollY + r.getBoundingClientRect().top - 300); return; }} s += pc.len + 1; }} }})()""")
    time.sleep(0.4)
    pt = br.js(f"""(() => {{ const v = {B}.view; let s = 0; for (const pc of v.pieces) {{ if (!pc.win && s <= {target} && {target} <= s + pc.len) {{
        const r = document.createRange(); r.setStart(pc.el.firstChild, {target} - s); r.setEnd(pc.el.firstChild, {target} - s + 1);
        const b = r.getBoundingClientRect(); return [b.left + 0.5, b.top + b.height / 2]; }} s += pc.len + 1; }} return null; }})()""")
    check("the target is static text", pt is not None)
    if pt:
        for t in ("mousePressed", "mouseReleased"):
            br.ws.call("Input.dispatchMouseEvent", {"type": t, "x": pt[0], "y": pt[1], "button": "left", "clickCount": 1, "modifiers": SHIFT})
        time.sleep(0.4)
        check("Shift+click in far static text selects from the caret to there", br.sel() == [60000, target], (br.sel(), target))
        key(br, "Delete", "Delete", 46)
        time.sleep(0.3)
        text = text[:60000] + text[target:]
        check("…and Delete deletes all of it", br.full() == text, len(br.full() or ""))
        time.sleep(1.6)
        ctrl(br, "z")
        time.sleep(0.3)
        check("…which undo brings back", br.full() is not None and len(br.full()) == len(text) + (target - 60000))
        text = br.full()
    # A 300 KB paste in the middle: the window grows, then shrinks back.
    big = u16(novel(300_000, seed=11))
    br.js(f"{B}.view.select(50000, 50000, true)")
    time.sleep(0.3)
    br.ws.call("Input.insertText", {"text": novel(300_000, seed=11)})
    time.sleep(1.5)
    text = text[:50000] + big + text[50000:]
    check("a 300 KB paste goes in", br.full() == text, len(br.full() or ""))
    ws, we = br.win()
    check("…and the window is cut back to its size", we - ws < 200_000, (ws, we))
    med, worst = br.per_key("after")
    check(f"…typing right after is quick ({med:.1f} ms a key)", med < 34, med)
    text = br.full()
    check("…and it all reaches the server", br.wait_synced(60) and server_text(db, pid) == text)


def visual(br, a):
    pid = new_post(a, "visual")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("Plain words here")
    time.sleep(1.6)
    br.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
    time.sleep(0.3)
    br.js("""(() => { const p = document.querySelector('.wys p'); const t = p.firstChild; const r = document.createRange();
        r.setStart(t, 6); r.setEnd(t, 11); getSelection().removeAllRanges(); getSelection().addRange(r); })()""")
    ctrl(br, "b")
    time.sleep(0.3)
    check("Visual mode: Ctrl+B makes the selection bold", br.full() == "Plain **words** here", repr(br.full()))
    ctrl(br, "z")
    time.sleep(0.3)
    check("…and Ctrl+Z undoes it, in the Markdown and on screen", br.full() == "Plain words here" and not br.js("!!document.querySelector('.wys strong')"), (br.full(), br.js("document.querySelector('.wys').innerHTML")))
    br.js("document.querySelector('.edit-tools .seg:nth-child(1)').click()")


def loading(br, db, a):
    pid = new_post(a, "loadshift")
    store(a, pid, novel(500_000, seed=5))
    br.js("localStorage.setItem('slop:vim', '0')")
    br.ws.call("Page.addScriptToEvaluateOnNewDocument", {"source":
        "window.__cls = 0; new PerformanceObserver((l) => { for (const e of l.getEntries()) if (!e.hadRecentInput) window.__cls += e.value; }).observe({type: 'layout-shift', buffered: true});"})
    open_post(br, pid)
    time.sleep(1)
    cls = br.js("window.__cls")
    check(f"a long post loads without layout shift (CLS {cls:.3f})", cls < 0.05, cls)


def phone(br, db, a):
    pid = new_post(a, "phone")
    open_post(br, pid)
    wide = br.js("document.documentElement.scrollWidth > innerWidth + 1")
    check("on a phone: nothing is wider than the screen", not wide, br.js("[document.documentElement.scrollWidth, innerWidth]"))
    check("…the Vim button is not offered", br.js("getComputedStyle(document.querySelector('.vim-toggle')).display") == "none")
    br.js("document.getElementById('editor').focus()")
    br.ws.call("Input.insertText", {"text": "Typed on a phone."})
    time.sleep(2.5)
    check("…typing works and syncs", br.full() == "Typed on a phone." and server_text(db, pid) == "Typed on a phone.", (br.full(), server_text(db, pid)))


def resize(br, db, a):
    pid = new_post(a, "resize")
    text = novel(400_000, seed=9)
    store(a, pid, text)
    text = u16(text)
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    at = text.index("\n\n", 200_000) + 2
    br.js(f"{B}.view.select({at}, {at}, true)")
    time.sleep(0.3)
    br.ws.call("Emulation.setDeviceMetricsOverride", {"width": 700, "height": 900, "deviceScaleFactor": 1, "mobile": False})
    time.sleep(0.5)
    br.js(f"{B}.view.reveal({at})")
    time.sleep(0.2)
    y = br.caret_y()
    check("after a resize the caret can be shown", 100 < y < 900, y)
    br.keys("Z")
    check("…and typing goes where it is", br.full() == text[:at] + "Z" + text[at:])
    br.ws.call("Emulation.setDeviceMetricsOverride", {"width": 1280, "height": 900, "deviceScaleFactor": 1, "mobile": False})


def vim_edges(br, db, a):
    pid = new_post(a, "vimedges")
    text = u16(novel(200_000, seed=4))
    store(a, pid, novel(200_000, seed=4))
    open_post(br, pid)
    br.js("localStorage.setItem('slop:vim-ok', '1')")
    br.js("document.querySelector('.vim-toggle').click()")
    time.sleep(0.2)
    br.js(f"{B}.view.select(100000, 100000, true)")
    time.sleep(0.3)
    ws, we = br.win()
    # dd on the window's last line, then P.
    last = text.rfind("\n", 0, we) + 1
    br.js(f"{B}.view.select({last}, {last + 1}, true)")
    time.sleep(0.3)
    ws2, we2 = br.win()
    br.keys("dd")
    time.sleep(0.2)
    e = text.find("\n", last)
    want = text[:last] + text[e + 1:]
    check("vim dd on a line at the window's edge", br.full() == want, br.full() == want)
    text = want
    br.keys("P")
    time.sleep(0.2)
    check("vim P puts the line back", br.full() is not None and len(br.full()) == len(text) + (e + 1 - last))
    text = br.full()
    # V, then j well past the window's edge, then d.
    br.js(f"{B}.view.select({ws2 + 200}, {ws2 + 201}, true)")
    time.sleep(0.2)
    at = br.sel()[0]
    br.keys("V" + "j" * 200 + "d")
    time.sleep(0.5)
    full = br.full()
    s0 = text.rfind("\n", 0, at) + 1
    check("vim V with j past the window's edge, then d: those lines go", full is not None and len(full) < len(text) - 10000 and full[:s0] == text[:s0], (len(full or ""), len(text)))
    br.keys("u")
    time.sleep(0.3)
    check("…and u brings them back", br.full() == text)
    br.js("document.querySelector('.vim-toggle').click()")


def shift_arrows(br, db, a):
    pid = new_post(a, "shiftarrows")
    store(a, pid, novel(150_000, seed=6))
    text = u16(novel(150_000, seed=6))
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.js(f"{B}.view.select(20000, 20000, true)")
    time.sleep(0.3)
    ws, we = br.win()
    for _ in range(420):
        key(br, "ArrowDown", "ArrowDown", 40, SHIFT)
    time.sleep(0.5)
    s = br.sel()
    check("Shift+ArrowDown held past the window's edge keeps selecting", s[0] == 20000 and s[1] > we, (s, we))
    br.keys("X")
    time.sleep(0.3)
    check("…and typing replaces the whole selection", br.full() == text[:20000] + "X" + text[s[1]:], len(br.full() or ""))


def reopen(br, db, a, tmp):
    """Offline edits, then the browser is shut down (not a page closed
    politely); opened again online, the post has them and sends them."""
    pid = new_post(a, "reopen")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("Kept. ")
    time.sleep(2)
    br.ws.call("Network.emulateNetworkConditions", {"offline": True, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})
    br.keys("Typed offline, browser closed.")
    time.sleep(1.5)
    br.close()
    time.sleep(1)
    b3 = page(9424, tmp, a, "c1")  # the same profile: its localStorage
    open_post(b3, pid)
    time.sleep(3)
    want = "Kept. Typed offline, browser closed."
    check("edits made offline, the browser then shut down, are sent when the post is opened again",
          b3.full() == want and server_text(db, pid) == want, (b3.full(), server_text(db, pid)))
    return b3


def visual_edit(br, b2, db, a):
    pid = new_post(a, "visualedit")
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.keys("First paragraph.<CR><CR>Second paragraph.")
    time.sleep(2)
    br.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
    time.sleep(0.3)
    # Enter at the end of the first paragraph, then a new one.
    br.js("""(() => { const p = document.querySelector('.wys p'); const t = p.firstChild; getSelection().collapse(t, t.length); })()""")
    br.keys("<CR>")
    br.js("document.execCommand('insertText', false, 'Middle one.')")
    time.sleep(0.4)
    check("Visual mode: Enter makes a new paragraph", br.full() == "First paragraph.\n\nMiddle one.\n\nSecond paragraph.", repr(br.full()))
    # Backspace at the start of the last paragraph merges it into the one before.
    br.js("""(() => { const ps = document.querySelectorAll('.wys p'); const t = ps[ps.length - 1].firstChild; getSelection().collapse(t, 0); })()""")
    br.keys("<BS>")
    time.sleep(0.4)
    check("…Backspace at a paragraph's start joins it to the one before", br.full() == "First paragraph.\n\nMiddle one.Second paragraph.", repr(br.full()))
    # Another writer, in Visual mode too.
    open_post(b2, pid)
    b2.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
    time.sleep(0.3)
    b2.js("""(() => { const p = document.querySelector('.wys p'); const t = p.firstChild; getSelection().collapse(t, 0); document.querySelector('.wys').focus(); })()""")
    b2.js("document.execCommand('insertText', false, 'Their ')")
    time.sleep(3)
    check("…another writer in Visual mode: their words appear here, rendered", "Their First paragraph." in (br.js("document.querySelector('.wys').textContent") or ""), br.js("document.querySelector('.wys').textContent"))
    check("…and both have the same text", br.full() == b2.full(), (br.full(), b2.full()))
    for b in (br, b2):
        b.js("document.querySelector('.edit-tools .seg:nth-child(1)').click()")


def long_line(br, db, a):
    pid = new_post(a, "longline")
    line = ("word " * 60000).strip()
    store(a, pid, line)
    open_post(br, pid)
    br.js("document.getElementById('editor').focus()")
    br.js(f"{B}.view.select(150000, 150000, true)")
    time.sleep(0.3)
    med, worst = br.per_key("abc")
    # A known limit (docs/FINDINGS.md in the old app): the window is cut at
    # line breaks only, and the browser lays a paragraph out whole.
    print(f"  (a 300 KB paragraph without line breaks: {med:.0f} ms a key)")
    check("a 300 KB paragraph without line breaks: typing works", br.full() == line[:150000] + "abc" + line[150000:])


def main():
    global BUDGET_DB
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    BUDGET_DB = dbpath
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_ORIGIN=BASE, BLOG_RP_ID="localhost")
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    brs = []
    only = sys.argv[1:]
    try:
        wait_port(PORT)
        db = sqlite3.connect(dbpath, timeout=10)
        _, a = user(db, "writer@example.com")
        http("POST", "/blogs", a, {"slug": "u", "title": "U"})
        br = page(9421, tmp, a, "c1")
        brs.append(br)
        b2 = page(9422, tmp, a, "c2")
        brs.append(b2)
        for name, fn, args in [("writing", writing, (br, db, a)), ("together", together, (br, b2, db, a)), ("offline", offline, (br, db, a)),
                               ("ime", ime, (br, a)), ("edges", edges, (br, db, a)), ("visual", visual, (br, a)),
                               ("loading", loading, (b2, db, a)), ("resize", resize, (br, db, a)),
                               ("vim at the edges", vim_edges, (br, db, a)), ("shift+arrows", shift_arrows, (br, db, a)),
                               ("visual editing", visual_edit, (br, b2, db, a)), ("long line", long_line, (br, db, a))]:
            if only and name not in only:
                continue
            print(f"-- {name}", flush=True)
            fresh_budget()
            fn(*args)
        if not only or "reopen" in only:
            print("-- reopen after the browser was shut down", flush=True)
            brs.remove(br)
            brs.append(reopen(br, db, a, tmp))
        if not only or "phone" in only:
            ph = page(9423, tmp, a, "c3", 390, 844, True)
            brs.append(ph)
            print("-- phone", flush=True)
            phone(ph, db, a)
    finally:
        for b in brs:
            b.close()
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
