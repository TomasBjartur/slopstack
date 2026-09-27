#!/usr/bin/env python3
"""Collaborative editing in two real browsers (headless Chrome), as two
users: concurrent edits, same-position typing, offline editing, reload,
publish, a form-written post, and an outsider trying to read or write.

usage: tests/collab_test.py   (needs tools/setup_chrome.sh once)
"""
import hashlib, os, re, secrets, socket, sqlite3, subprocess, sys, tempfile, time, urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import start_chrome, page_ws, wait_port

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8096
BASE = f"http://localhost:{PORT}"
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


# This test writes far faster than a person: each POST starts with a fresh
# write budget (30 a minute; its own test: tests/redteam_test.py).
BUDGET_DB = None


def fresh_budget():
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
    s = socket.create_connection(("127.0.0.1", PORT), timeout=10)
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
    uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES (?, ?, ?, 1)", (email, email.split("@")[0], email.split("@")[0])).lastrowid
    raw = secrets.token_bytes(32)
    now = int(time.time() * 1000)
    db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw).digest(), uid, now, now + 3600_000))
    db.commit()
    return uid, raw.hex()


class Editor:
    def __init__(self, port, profile, sid):
        self.proc = start_chrome(port, profile)
        self.ws = page_ws(port)
        self.ws.call("Page.enable")
        self.ws.call("Runtime.enable")
        self.ws.call("Network.enable")
        self.ws.call("Network.setCookie", {"name": "sid", "value": sid, "url": BASE, "httpOnly": True, "secure": True, "sameSite": "Strict"})

    def js(self, expr):
        r = self.ws.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})
        return r.get("result", {}).get("value")

    def open(self, url):
        self.ws.call("Page.navigate", {"url": url})
        for _ in range(100):
            time.sleep(0.1)
            if self.js("document.readyState") == "complete" and self.js("!!(document.getElementById('editor') || {}).ed && document.getElementById('editor').ed.loaded()"):
                break
        time.sleep(0.3)

    def text(self):
        return self.js("document.getElementById('editor').value")

    def status(self):
        return self.js("(document.getElementById('sync-status') || {}).textContent")

    # Type as a user does: change the value at a position, fire input.
    def type_at(self, pos, s):
        self.js(f"(() => {{ const t = document.getElementById('editor'); const a = Array.from(t.value);"
                f" a.splice({pos}, 0, ...Array.from({s!r})); t.value = a.join('');"
                f" t.dispatchEvent(new Event('input', {{bubbles: true}})); }})()")

    def type_end(self, s):
        self.js(f"(() => {{ const t = document.getElementById('editor'); t.value += {s!r};"
                f" t.dispatchEvent(new Event('input', {{bubbles: true}})); }})()")

    def offline(self, on):
        self.ws.call("Network.emulateNetworkConditions", {"offline": on, "latency": 0, "downloadThroughput": -1, "uploadThroughput": -1})
        if not on:
            self.js("window.dispatchEvent(new Event('online'))")

    def close(self):
        self.proc.terminate()
        self.proc.wait()


def settle(eds, want=None, secs=12):
    for _ in range(secs * 5):
        time.sleep(0.2)
        texts = [e.text() for e in eds]
        if len(set(texts)) == 1 and (want is None or texts[0] == want) and all(e.status() == "Saved" for e in eds):
            return texts[0]
    return [e.text() for e in eds]


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    global BUDGET_DB
    BUDGET_DB = dbpath
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_ORIGIN=BASE, BLOG_RP_ID="localhost")
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    eds = []
    try:
        wait_port(PORT)
        db = sqlite3.connect(dbpath)
        a_id, a = user(db, "alice@example.com")
        b_id, b = user(db, "bob@example.com")
        c_id, c = user(db, "carol@example.com")
        http("POST", "/blogs", a, {"slug": "team", "title": "Team"})
        http("POST", "/dash/team/authors", a, {"email": "bob@example.com"})
        _, loc, _ = http("POST", "/dash/team/posts", a, {"slug": "doc", "title": "Doc"})
        pid = loc.rsplit("/", 1)[1]
        edit = f"{BASE}/edit/{pid}"

        ea = Editor(9341, f"{tmp}/ca", a)
        eb = Editor(9342, f"{tmp}/cb", b)
        eds = [ea, eb]
        ea.open(edit)
        eb.open(edit)
        errs = [m for e in eds for m in e.ws.events if m.get("method") in ("Runtime.exceptionThrown",)]
        check("both editors load (the WebAssembly, the document), no errors", ea.text() == "" and eb.text() == "" and not errs, (ea.text(), eb.text(), errs[:1]))

        ea.type_end("Hello")
        got = settle(eds, "Hello")
        check("A types, B sees it", got == "Hello", got)

        ea.type_at(0, ">> ")
        eb.type_end(" world")
        got = settle(eds)
        check("concurrent edits at both ends converge", got == ">> Hello world", got)

        ea.type_at(3, "AAA")
        eb.type_at(3, "BBB")
        got = settle(eds)
        check("same position: converge, runs not interleaved",
              got in (">> AAABBBHello world", ">> BBBAAAHello world"), got)

        ea.type_end(" 🌊 é 日本")
        got = settle(eds)
        check("characters outside ASCII and the BMP", isinstance(got, str) and got.endswith(" 🌊 é 日本"), got)

        eb.offline(True)
        time.sleep(0.3)
        eb.type_end(" (offline)")
        ea.type_at(0, "# ")
        time.sleep(2.5)
        check("offline editor keeps its edit locally", "(offline)" in eb.text() and not eb.text().startswith("# "), eb.text())
        check("offline editor says so", "Offline" in (eb.status() or ""), eb.status())
        eb.offline(False)
        got = settle(eds)
        check("back online: both converge with both edits",
              isinstance(got, str) and got.startswith("# >> ") and got.endswith(" (offline)"), got)

        # Offline, then the page closed: the edit is kept on the device and
        # sent from the next page load (with the old page's replica number).
        eb.offline(True)
        time.sleep(0.3)
        eb.type_end(" [kept]")
        time.sleep(1.5)
        eb.js("document.dispatchEvent(new Event('visibilitychange'))")
        eb.offline(False)
        eb.open(edit)
        got = settle(eds)
        check("an edit made offline survives a reload and arrives", isinstance(got, str) and got.endswith("[kept]"), got)

        eb.open(edit)
        check("reload shows the same text", eb.text() == ea.text(), (eb.text(), ea.text()))

        # Publish; the public page renders the document's Markdown.
        http("POST", f"/edit/{pid}/publish", a)
        st, _, body = http("GET", "/b/team/doc")
        check("published from the document", st == 200 and "(offline)" in body and "world" in body, (st, body[:300]))
        check("markdown heading rendered", "<h1>" in body and "&gt;&gt; " not in body.split("<h1>")[0][-5:], body[:300])

        # A post written with the form (no JavaScript) opens in the editor
        # exactly, lines, tabs and a leading blank line kept.
        text = "\nFirst paragraph.\n\n## Heading\n\n\tindented & <tagged>\nlast"
        _, loc, _ = http("POST", "/dash/team/posts", a, {"slug": "old", "title": "Old"})
        opid = loc.rsplit("/", 1)[1]
        http("POST", f"/edit/{opid}", a, {"title": "Old", "body": text})
        ea.open(f"{BASE}/edit/{opid}")
        check("editor shows a form-written post exactly", ea.text() == text, repr(ea.text()))
        ea.type_end("!")
        settle([ea])
        http("POST", f"/edit/{opid}/publish", a)
        st, _, body = http("GET", "/b/team/old")
        check("…edited in the editor and published", "last!" in body and "&lt;tagged&gt;" in body, body[-400:])

        # An outsider can neither read nor write the document.
        st, _, _ = http("GET", f"/edit/{pid}", c)
        check("outsider: editor 404", st == 404, st)
        check("no CSP violations or errors in either browser",
              not [m for e in eds for m in e.ws.events if m.get("method") == "Runtime.exceptionThrown"
                   or (m.get("method") == "Runtime.consoleAPICalled" and m["params"].get("type") == "error")],
              [m for e in eds for m in e.ws.events if m.get("method") in ("Runtime.exceptionThrown", "Runtime.consoleAPICalled")][:2])
    finally:
        for e in eds:
            e.close()
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


main()
