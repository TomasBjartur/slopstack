#!/usr/bin/env python3
"""Comments, likes and images: flows and attacks over HTTP, then in real
Chrome (a comment appearing live in another browser, an instant like,
replies in place, "More comments", an image pasted into the editor).
usage: tests/social_test.py"""
import hashlib, json, os, re, secrets, socket, sqlite3, struct, subprocess, sys, tempfile, time, urllib.parse, zlib

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import start_chrome, page_ws, wait_port

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8086
BASE = f"http://localhost:{PORT}"
fails = 0
DB = None


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def http(method, path, sid=None, form=None, raw=None, ctype=None, site="same-origin", ds=False, budget=True):
    if method == "POST" and budget and DB:
        c = sqlite3.connect(DB, timeout=5)
        c.execute("DELETE FROM write_budget")
        c.commit()
        c.close()
    body = raw if raw is not None else (urllib.parse.urlencode(form).encode() if form is not None else b"")
    h = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
    if site:
        h += f"Sec-Fetch-Site: {site}\r\n"
    if sid:
        h += f"Cookie: sid={sid}\r\n"
    if ds:
        h += "Datastar-Request: true\r\n"
    if method == "POST":
        h += f"Content-Type: {ctype or 'application/x-www-form-urlencoded'}\r\nContent-Length: {len(body)}\r\n"
    s = socket.create_connection(("127.0.0.1", PORT), timeout=20)
    s.sendall(h.encode() + b"\r\n" + body)
    out = b""
    while True:
        b = s.recv(1 << 16)
        if not b:
            break
        out += b
    s.close()
    head, _, rb = out.partition(b"\r\n\r\n")
    loc = re.search(rb"\r\nLocation: ([^\r]*)", head)
    return int(head.split(b" ")[1]), (loc.group(1).decode() if loc else None), rb, head.decode("latin-1")


def user(db, email, name):
    uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES (?, ?, ?, 1)", (email, name, name.lower())).lastrowid
    raw = secrets.token_bytes(32)
    now = int(time.time() * 1000)
    db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw).digest(), uid, now, now + 3600_000))
    db.commit()
    return uid, raw.hex()


def png(w=2, h=2):
    raw = b"".join(b"\x00" + b"\xff\x00\x00" * w for _ in range(h))
    def chunk(t, d):
        return struct.pack(">I", len(d)) + t + d + struct.pack(">I", zlib.crc32(t + d))
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) + chunk(b"IDAT", zlib.compress(raw)) + chunk(b"IEND", b"")


def run(db):
    a_id, a = user(db, "ann@example.com", "Ann")
    b_id, b = user(db, "ben@example.com", "Ben")
    m_id, m = user(db, "mal@example.com", "Mal")
    http("POST", "/blogs", a, {"slug": "ann", "title": "Ann"})
    _, loc, _, _ = http("POST", "/dash/ann/posts", a, {"title": "Live", "slug": "live"})
    pid = int(loc.rsplit("/", 1)[1])
    _, loc2, _, _ = http("POST", "/dash/ann/posts", a, {"title": "Draft", "slug": "draft"})
    did = int(loc2.rsplit("/", 1)[1])
    http("POST", f"/edit/{pid}", a, {"title": "Live", "body": "Hello readers.", "action": "publish"})

    # COMMENTS over HTTP.
    st, body, _, _ = http("GET", "/b/ann/live")[0], None, None, None
    st, _, body, _ = http("GET", "/b/ann/live")
    check("a published post has a comments section and a like button", st == 200 and b'id="comments"' in body and b'class="btn like"' in body)
    st, loc, _, _ = http("POST", f"/comment/{pid}", b, {"body": "First! <script>alert(1)</script> **bold**", "parent": "0"})
    check("comment: back to it", st == 303 and loc.startswith("/b/ann/live#c"), (st, loc))
    cid = int(loc.rsplit("#c", 1)[1])
    _, _, body, _ = http("GET", "/b/ann/live")
    check("comment shown, Markdown rendered, script inert", b"<strong>bold</strong>" in body and b"<script>alert" not in body and b"&lt;script&gt;" in body, body[-900:])
    check("comment count on the post", b"1 comment<" in body)
    st, loc, _, _ = http("POST", f"/comment/{pid}", a, {"body": "A reply", "parent": str(cid)})
    rid = int(loc.rsplit("#c", 1)[1])
    _, _, body, _ = http("GET", "/b/ann/live")
    i_parent, i_reply = body.find(f'id="c{cid}"'.encode()), body.find(f'id="c{rid}"'.encode())
    check("a reply nested under its parent", 0 < i_parent < i_reply and body.find(f'id="r{cid}"'.encode()) < i_reply)
    st, _, _, _ = http("POST", f"/comment/{did}", b, {"body": "on a draft", "parent": "0"})
    check("cannot comment on a draft", st == 403, st)
    st, _, _, _ = http("POST", f"/comment/{pid}", None, {"body": "anon", "parent": "0"})
    check("anonymous: cannot comment", st == 403, st)
    st, _, _, _ = http("POST", f"/comment/{pid}", b, {"body": "   ", "parent": "0"})
    check("blank comment: 400", st == 400, st)
    st, _, _, _ = http("POST", f"/comment/{pid}", b, {"body": "x" * 10001, "parent": "0"})
    check("a comment over 10,000 characters: 400", st == 400, st)
    st, _, _, _ = http("POST", f"/comment/{pid}", b, {"body": "cross", "parent": "0"}, site="cross-site")
    check("CSRF: a cross-site comment is refused", st == 403, st)
    _, loc3, _, _ = http("POST", "/dash/ann/posts", a, {"title": "Other", "slug": "other"})
    oid = int(loc3.rsplit("/", 1)[1])
    http("POST", f"/edit/{oid}", a, {"title": "Other", "body": "x", "action": "publish"})
    st, _, _, _ = http("POST", f"/comment/{oid}", b, {"body": "reply across posts", "parent": str(cid)})
    check("a reply to a comment on another post: 400", st == 400, st)
    # Deleting.
    st, _, _, _ = http("POST", f"/comment/{cid}/delete", m)
    check("an outsider cannot delete someone's comment", st == 403, st)
    st, _, _, _ = http("POST", f"/comment/{rid}/delete", b)
    check("…nor can another commenter", st == 403, st)
    st, _, _, _ = http("POST", f"/comment/{cid}/delete", b)
    _, _, body, _ = http("GET", "/b/ann/live")
    check("the author deletes their comment: text gone, reply kept", st == 303 and b"First!" not in body and b"[deleted]" in body and b"A reply" in body, st)
    _, loc4, _, _ = http("POST", f"/comment/{pid}", m, {"body": "spam spam", "parent": "0"})
    sid_ = int(loc4.rsplit("#c", 1)[1])
    st, _, _, _ = http("POST", f"/comment/{sid_}/delete", a)
    _, _, body, _ = http("GET", "/b/ann/live")
    check("an author of the blog moderates", st == 303 and b"spam spam" not in body, st)
    st, _, _, _ = http("POST", "/comment/999999/delete", a)
    check("deleting a comment that does not exist: 404", st == 404, st)
    # Budget.
    codes = [http("POST", f"/comment/{pid}", m, {"body": f"flood {i}", "parent": "0"}, budget=False)[0] for i in range(35)]
    check("comments share the write budget (30 a minute): then 429", codes.count(303) == 30 and codes[-1] == 429, codes)

    # Datastar: a comment's answer brings everything after the cursor.
    last = db.execute("SELECT max(id) FROM comment").fetchone()[0]
    st, _, body, head = http("POST", f"/comment/{pid}", b, {"body": "Via Datastar", "parent": "0", "after": str(last)}, ds=True)
    check("Datastar comment: the new comment appended, the box emptied, the cursor moved",
          st == 200 and b"selector #thread" in body and b"mode append" in body and b"Via Datastar" in body and b"selector #cform" in body
          and b'"cafter": ' in body, body[:500])
    st, _, body, _ = http("GET", f"/live/{pid}?after={last}&n=3", ds=True)
    check("live: comments after the cursor, and the next poll", st == 200 and b"Via Datastar" in body and b"n=4" in body, body[:300])
    st, _, body, _ = http("GET", f"/live/{did}?after=0", a, ds=True)
    check("live: nothing for a draft", st in (403, 404) or b"Not allowed" in body or b"did not work" in body, (st, body[:200]))
    # More comments.
    _, _, body, _ = http("GET", "/b/ann/live")
    n_threads = body.count(b'data-parent="0"')
    check("the post page shows 20 threads, then More comments", n_threads == 20 and b'id="more-comments"' in body, n_threads)
    after = re.search(rb"/comments/%d\?after=(\d+)" % pid, body).group(1).decode()
    st, _, more, _ = http("GET", f"/comments/{pid}?after={after}", ds=True)
    check("More comments: the next threads before the button", st == 200 and b"selector #more-comments" in more and b"mode before" in more and more.count(b'data-parent="0"') >= 10, more[:300])

    # LIKES.
    st, loc, _, _ = http("POST", f"/like/{pid}", b, {"on": "1"})
    _, _, body, _ = http("GET", "/b/ann/live", b)
    check("like: counted, shown as liked", st == 303 and b'class="like on"' in body and b">1</span>" in body, st)
    http("POST", f"/like/{pid}", b, {"on": "1"})
    _, _, body, _ = http("GET", "/b/ann/live")
    check("liking twice counts once", b"\xe2\x99\xa5 1<" in body, body[body.find(b'class="btn like"'):][:80])
    st, _, body, _ = http("POST", f"/like/{pid}", b, {"on": "0"}, ds=True)
    check("unlike (Datastar): the bar from the server", st == 200 and b'id="social"' in body and b"_ln: 0" in body, body[:400])
    st, _, _, _ = http("POST", f"/like/{did}", b, {"on": "1"})
    check("cannot like a draft", st == 403, st)
    st, _, _, _ = http("POST", f"/like/{pid}", None, {"on": "1"})
    check("anonymous: cannot like", st == 403, st)

    # IMAGES.
    img = png()
    st, _, body, _ = http("POST", f"/upload/{did}", a, raw=img, ctype="image/png")
    key = body.decode().strip()
    check("an author uploads an image", st == 200 and re.fullmatch(r"/img/[0-9a-f]{32}", key), (st, body[:100]))
    st, _, got, head = http("GET", key)
    check("served as what its bytes are, cached for good, no sniffing", st == 200 and got == img and "image/png" in head and "immutable" in head and "nosniff" in head, head)
    st, _, _, _ = http("POST", f"/upload/{did}", m, raw=img, ctype="image/png")
    check("an outsider cannot upload to someone's post", st == 403, st)
    st, _, _, _ = http("POST", f"/upload/{did}", a, raw=b"<svg xmlns='http://www.w3.org/2000/svg'><script>alert(1)</script></svg>", ctype="image/svg+xml")
    check("SVG (script inside) refused", st == 415, st)
    st, _, _, _ = http("POST", f"/upload/{did}", a, raw=b"<html><script>alert(1)</script>", ctype="image/png")
    check("HTML claiming to be a PNG refused", st == 415, st)
    st, _, _, _ = http("POST", f"/upload/{did}", a, raw=b"\x89PNG\r\n\x1a\n" + b"\0" * (2 << 20), ctype="image/png")
    check("an image over 2 MiB refused", st == 413, st)
    st, _, _, _ = http("POST", f"/upload/{did}", a, raw=img, ctype="image/png", site="cross-site")
    check("CSRF: a cross-site upload refused", st == 403, st)
    for bad in ["/img/" + "0" * 32, "/img/ABCDEF" + "0" * 26, "/img/../etc"]:
        check(f"no image at {bad}", http("GET", bad)[0] == 404)
    return pid, did, a, b


def chrome_part(db, tmp, pid, did, a, b):
    class Br:
        def __init__(self, port, sid):
            self.proc = start_chrome(port, f"{tmp}/c{port}")
            self.ws = page_ws(port)
            for d in ("Page", "Runtime", "Network", "Log"):
                self.ws.call(d + ".enable")
            self.ws.call("Network.setCookie", {"name": "sid", "value": sid, "url": BASE, "httpOnly": True, "sameSite": "Strict"})

        def js(self, e):
            return self.ws.call("Runtime.evaluate", {"expression": e, "awaitPromise": True, "returnByValue": True}).get("result", {}).get("value")

        def open(self, url):
            self.ws.call("Page.navigate", {"url": url})
            for _ in range(100):
                time.sleep(0.1)
                if self.js("document.readyState") == "complete":
                    break
            time.sleep(0.5)

        def until(self, e, secs=15):
            for _ in range(int(secs * 5)):
                v = self.js(e)
                if v:
                    return v
                time.sleep(0.2)
            return None

    x, y = Br(9441, a), Br(9442, b)
    try:
        x.open(f"{BASE}/b/ann/live")
        y.open(f"{BASE}/b/ann/live")
        x.js("window.__m = 1")
        # B comments with Datastar; A sees it live (the poll), B at once.
        y.js("{ const t = document.querySelector('#cform textarea'); t.value = 'Hello from Chrome B'; document.querySelector('#cform button').click() }")
        seen_b = y.until("document.getElementById('thread').innerText.includes('Hello from Chrome B') && document.querySelector('#cform textarea').value === ''", 5)
        check("B's comment shows at once, the box emptied, no reload", seen_b and y.js("performance.getEntriesByType('navigation').length") == 1)
        seen_a = x.until("document.getElementById('thread').innerText.includes('Hello from Chrome B')", 15)
        check("…and appears live in A's browser within 15 s, no reload", seen_a and x.js("window.__m") == 1)
        n = y.js("[...document.querySelectorAll('.comment')].filter(c => c.innerText.includes('Hello from Chrome B')).length")
        y.until("false", 11)
        n2 = y.js("[...document.querySelectorAll('.comment')].filter(c => c.innerText.includes('Hello from Chrome B')).length")
        check("the live poll does not show it twice", n == 1 and n2 == 1, (n, n2))
        # A like shows before the server answers.
        y.js("window.__m = 2")
        before = y.js("document.querySelector('#social .like span').textContent")
        y.js("document.querySelector('#social button.like').click()")
        instant = y.js("document.querySelector('#social .like span').textContent")
        time.sleep(1.0)
        after = y.js("document.querySelector('#social .like span').textContent")
        server = db.execute("SELECT like_count FROM post WHERE id = ?", (pid,)).fetchone()[0]
        check("a like counts at once, and the server agrees", instant == str(int(before) + 1) and after == instant and server == int(after) and y.js("window.__m") == 2, (before, instant, after, server))
        # Reply in place.
        y.js("document.querySelector('.cactions a[href^=\"/reply/\"]').click()")
        got = y.until("!!document.querySelector('.rf form textarea')", 5)
        if got:
            y.js("{ const t = document.querySelector('.rf form textarea'); t.value = 'A reply in place'; document.querySelector('.rf form button.primary').click() }")
        placed = y.until("[...document.querySelectorAll('.replies .comment')].some(c => c.innerText.includes('A reply in place'))", 5)
        time.sleep(1.0)
        check("…once", y.js("[...document.querySelectorAll('.comment')].filter(c => c.querySelector(':scope > .cbody').innerText.includes('A reply in place')).length") == 1)
        check("reply: the box opens in place, the reply lands under its comment", got and placed and y.js("window.__m") == 2,
              (bool(got), bool(placed), y.js("window.__m"), (y.js("[...document.querySelectorAll('.replies .comment')].map(c => c.innerText.slice(0, 40)).join(' | ')") or "")[:300]))
        # More comments in place.
        y.js("window.__m = 3")
        before = y.js("document.querySelectorAll('#thread .comment[data-parent=\"0\"]').length")
        y.js("document.querySelector('#more-comments a').click()")
        more = y.until(f"document.querySelectorAll('#thread .comment[data-parent=\"0\"]').length > {before} ? document.querySelectorAll('#thread .comment[data-parent=\"0\"]').length : 0", 5)
        check("More comments load in place", more and more > before and y.js("window.__m") == 3, (before, more))
        # An image pasted into the editor becomes Markdown and uploads.
        x.open(f"{BASE}/edit/{did}")
        x.until("document.getElementById('editor').ed && document.getElementById('editor').ed.loaded()", 10)
        x.js("""(async () => { const c = document.createElement('canvas'); c.width = 40; c.height = 30; c.getContext('2d').fillRect(0, 0, 20, 20);
             const blob = await new Promise(r => c.toBlob(r, 'image/png')); const f = new File([blob], 'my photo.png', {type: 'image/png'});
             const dt = new DataTransfer(); dt.items.add(f); const t = document.getElementById('editor'); t.focus();
             t.dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true})); })()""")
        md = x.until("(document.getElementById('editor').ed.view.text.match(/!\\[[^\\]]*\\]\\(\\/img\\/[0-9a-f]{32}\\)/) || [])[0]", 10)
        check("a pasted image uploads and goes in as Markdown", md is not None, x.js("document.getElementById('editor').ed.view.text"))
        errs = [e for br in (x, y) for e in br.ws.events if e.get("method") == "Runtime.exceptionThrown"
                or (e.get("method") == "Log.entryAdded" and e["params"]["entry"]["level"] == "error")]
        check("no errors or CSP violations in either browser", not errs, errs[:2])
    finally:
        x.proc.terminate()
        y.proc.terminate()


def main():
    global DB
    tmp = tempfile.mkdtemp()
    DB = os.path.join(tmp, "blog.db")
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=dict(os.environ, PORT=str(PORT), BLOG_DB=DB, BLOG_ORIGIN=BASE, BLOG_RP_ID="localhost"),
                           stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        wait_port(PORT)
        db = sqlite3.connect(DB, timeout=5)
        pid, did, a, b = run(db)
        chrome_part(db, tmp, pid, did, a, b)
    finally:
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


main()
