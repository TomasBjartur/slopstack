#!/usr/bin/env python3
"""The editor's sync protocol against build/server: operations stored and
handed on, repeats harmless, replica numbers bound to their writer, bad
batches refused whole, snapshots, publishing from the document, the form
without JavaScript. A Python client plays the editor (src/crdt.rs wire
format). usage: tests/sync_test.py"""
import hashlib, os, re, secrets, socket, sqlite3, struct, subprocess, sys, tempfile, time, urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8097
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def http(method, path, sid=None, body=b"", ctype=None, site="same-origin"):
    h = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
    if sid:
        h += f"Cookie: sid={sid}\r\n"
    if site:
        h += f"Sec-Fetch-Site: {site}\r\n"
    if method == "POST":
        h += f"Content-Type: {ctype or 'application/x-www-form-urlencoded'}\r\nContent-Length: {len(body)}\r\n"
    s = socket.create_connection(("127.0.0.1", PORT), timeout=30)
    s.sendall(h.encode() + b"\r\n" + body)
    out = b""
    while True:
        b = s.recv(1 << 20)
        if not b:
            break
        out += b
    s.close()
    head, _, rb = out.partition(b"\r\n\r\n")
    st = int(head.split(b" ")[1]) if head.startswith(b"HTTP/1.1 ") else 0
    loc = re.search(rb"\r\nLocation: ([^\r]*)", head)
    return st, rb, (loc.group(1).decode() if loc else None)


def form(d):
    return urllib.parse.urlencode(d).encode()


def ins(rep, ctr, prep, pctr, side, text):
    t = text.encode()
    return struct.pack("<BIIIIBI", 1, rep, ctr, prep, pctr, side, len(t)) + t


def dele(rep, ctr, n):
    return struct.pack("<BIII", 2, rep, ctr, n)


def sync(sid, post, since, rep, ops=b"", me=None):
    """me: the asking page's own replica (default: rep, as a page sends its own)."""
    st, body, _ = http("POST", f"/edit/{post}/sync?me={rep if me is None else me}", sid, struct.pack("<QI", since, rep) + ops, "application/octet-stream")
    if st != 200:
        return st, None
    kind, seq, more = body[0], struct.unpack("<Q", body[1:9])[0], body[9]
    rest = body[10:]
    snap = None
    if kind == 1:
        n = struct.unpack("<I", rest[:4])[0]
        snap, rest = rest[4:4 + n], rest[4 + n:]
    return st, {"seq": seq, "more": more, "snap": snap, "ops": rest}


def user(db, email, name):
    uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES (?, ?, ?, 1)", (email, name, name.lower())).lastrowid
    raw = secrets.token_bytes(32)
    now = int(time.time() * 1000)
    db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw).digest(), uid, now, now + 3600_000))
    db.commit()
    return uid, raw.hex()


def rep_of(sid, post):
    st, body, _ = http("GET", f"/edit/{post}", sid)
    m = re.search(rb'data-rep="(\d+)"', body)
    return st, (int(m.group(1)) if m else None), body.decode("utf-8", "replace")


def run(db):
    a_id, a = user(db, "ann@example.com", "Ann")
    b_id, b = user(db, "ben@example.com", "Ben")
    _, m = user(db, "mal@example.com", "Mal")
    http("POST", "/blogs", a, form({"slug": "ann", "title": "Ann"}))
    http("POST", "/dash/ann/authors", a, form({"email": "ben@example.com"}))
    _, _, loc = http("POST", "/dash/ann/posts", a, form({"title": "Shared"}))
    post = int(loc.rsplit("/", 1)[1])

    st, ra, page = rep_of(a, post)
    _, ra2, _ = rep_of(a, post)
    _, rb, _ = rep_of(b, post)
    check("each editor page gets its own replica number", st == 200 and ra and ra2 and rb and len({ra, ra2, rb}) == 3 and min(ra, ra2, rb) >= 2, (ra, ra2, rb))

    st, r = sync(a, post, 0, ra)
    check("an empty document", st == 200 and r["seq"] == 0 and r["ops"] == b"" and r["snap"] is None, (st, r))
    batch = ins(ra, 1, 0, 0, 1, "hello")
    st, r = sync(a, post, 0, ra, batch)
    check("a batch stored, and not sent back to its writer (it has it)", st == 200 and r["seq"] > 0 and r["ops"] == b"", (st, r))
    seq1 = r["seq"]
    st, r = sync(b, post, 0, rb)
    check("a co-author gets it", st == 200 and r["ops"] == batch and r["seq"] == seq1, (st, r))
    st, r = sync(a, post, seq1, ra, batch)
    check("a repeated batch (a retry): not stored again", st == 200 and r["seq"] == seq1 and r["ops"] == b"", (st, r))
    b2 = ins(rb, 1, ra, 5, 1, " world")
    st, r = sync(b, post, seq1, rb, b2)
    check("the co-author's batch, stored", st == 200 and r["ops"] == b"" and r["seq"] > seq1, (st, r))
    st, r = sync(a, post, seq1, ra)
    check("…reaches the first writer", st == 200 and r["ops"] == b2, (st, r))
    # A new page load restoring unsent edits sends them as its old replica
    # but is a new one (me): the old replica's stored batches are not its
    # own, and it gets them (found in Chrome: a reopened page stayed empty).
    st, r = sync(a, post, 0, ra, me=ra2)
    check("a page sending an earlier page's edits still gets that page's stored ones", st == 200 and batch in r["ops"] and b2 in r["ops"], (st, r))
    seq2 = r["seq"]

    # Attacks and mistakes: refused whole, nothing stored.
    def refused(name, sid, rep, ops, want):
        before = db.execute("SELECT count(*) FROM doc_ops").fetchone()[0]
        st, _ = sync(sid, post, seq2, rep, ops)
        after = db.execute("SELECT count(*) FROM doc_ops").fetchone()[0]
        check(f"refused ({want}): {name}", st == want and after == before, (st, before, after))

    refused("inserting with another writer's replica number", b, rb, ins(ra, 50, 0, 0, 1, "x"), 400)
    refused("using a replica number given to someone else", b, ra, ins(ra, 50, 0, 0, 1, "x"), 403)
    refused("a replica number never given", a, 999, ins(999, 1, 0, 0, 1, "x"), 403)
    refused("an outsider", m, ra, ins(ra, 50, 0, 0, 1, "x"), 403)
    refused("nobody signed in", None, ra, ins(ra, 50, 0, 0, 1, "x"), 403)
    refused("the same ids with other text", a, ra, ins(ra, 1, 0, 0, 1, "HELLO"), 400)
    refused("a missing parent", a, ra, ins(ra, 50, 77, 77, 1, "x"), 400)
    refused("deleting what is not there", a, ra, dele(ra, 400, 2), 400)
    refused("a good operation, then a bad one: all refused", a, ra, ins(ra, 6, ra, 5, 1, "!") + b"\x07", 400)
    refused("garbage", a, ra, b"\x01\x02\x03", 400)
    st, _, _ = http("POST", f"/edit/{post}/sync", a, struct.pack("<QI", 0, ra) + ins(ra, 60, 0, 0, 1, "x"), "application/octet-stream", site="cross-site")
    check("CSRF: a cross-site sync is refused", st == 403, st)
    st, _, _ = http("POST", f"/edit/{post}/sync", a, b"short", "application/octet-stream")
    check("a body too short: 400", st == 400, st)
    st, r = sync(a, post, seq2, ra, ins(ra, 6, ra, 5, 1, "!"))
    check("after all that, an honest batch still works", st == 200 and r["seq"] > seq2, (st, r))
    st, r = sync(b, post, seq2, rb)
    check("…and reaches the co-author", st == 200 and ins(ra, 6, ra, 5, 1, "!") in r["ops"], (st, r))
    seq3 = r["seq"]

    # The draft page and the preview are rendered once per version of the
    # document (cached by its seq): an edit shows at once.
    slug = db.execute("SELECT slug FROM post WHERE id = ?", (post,)).fetchone()[0]
    views = lambda: (http("GET", f"/b/ann/{slug}", a)[1], http("GET", f"/edit/{post}/preview", a)[1])
    before = views()
    st, r = sync(a, post, seq3, ra, ins(ra, 100, ra, 6, 1, " fresh"))
    after = views()
    check("the draft page and the preview show an edit made after they were cached",
          all(b"fresh" not in v for v in before) and all(b"hello! fresh world" in v for v in after), [v[-400:] for v in after])
    st, r = sync(a, post, r["seq"], ra, dele(ra, 100, 6))
    check("…and when it is undone", all(b"fresh" not in v and b"hello! world" in v for v in views()))
    seq3 = r["seq"]

    # Removed from the blog: syncing stops.
    http("POST", f"/dash/ann/authors/{b_id}/remove", a)
    st, _ = sync(b, post, seq3, rb, ins(rb, 50, 0, 0, 1, "late"))
    check("a removed author can no longer sync", st == 403, st)
    http("POST", "/dash/ann/authors", a, form({"email": "ben@example.com"}))

    # The draft page and publishing show the document's text.
    st, body, _ = http("GET", "/b/ann/" + db.execute("SELECT slug FROM post WHERE id = ?", (post,)).fetchone()[0], a)
    check("the draft page shows the document", st == 200 and b"hello! world" in body, body[-300:])
    st, _, loc = http("POST", f"/edit/{post}", a, form({"title": "Shared", "action": "publish"}))
    st, body, _ = http("GET", loc)
    check("published from the document", st == 200 and b"hello! world" in body, (st, loc))

    # Without JavaScript: the form's text replaces the document's, as
    # operations of the server's replica that editors then receive.
    st, _, _ = http("POST", f"/edit/{post}", a, form({"title": "Shared", "body": "hello there world"}))
    st, r = sync(a, post, seq3, ra)
    check("the form's text arrives as operations of the server's replica (1)", st == 200 and b"\x01\x01\x00\x00\x00" in r["ops"] and r["ops"].endswith(b" there"), (st, r))
    st, r = sync(b, post, 0, rb)
    _, _, page = rep_of(a, post)
    check("…and the edit page shows it", "hello there world" in page)
    check("readers still see the published text until Update", b"hello! world" in http("GET", loc)[1])

    # Snapshots: after more than 1 MiB of operations, a new editor starts
    # from a snapshot plus what came after.
    _, rc, _ = rep_of(a, post)
    ctr = 1
    for i in range(5):
        chunk = ("line %d " % i) * 30000 + "\n"
        prev = (0, 0) if i == 0 else (rc, ctr - 1)
        sync(a, post, 0, rc, ins(rc, ctr, prev[0], prev[1], 1, chunk))
        ctr += len(chunk)
    snaps = db.execute("SELECT upto, size FROM doc_snap WHERE post_id = ?", (post,)).fetchall()
    check("a snapshot was written", len(snaps) == 1 and snaps[0][1] > 1_000_000, snaps)
    st, r = sync(b, post, 0, rb)
    check("a new editor gets the snapshot", st == 200 and r["snap"] is not None and r["snap"][:4] == b"FUG1", (st, r and r["seq"]))
    _, _, page = rep_of(a, post)
    check("a long text is not put in the edit page, and the form cannot replace it", 'data-big="1"' in page and 'name="body"' not in page)


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath)
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        for _ in range(50):
            try:
                socket.create_connection(("127.0.0.1", PORT), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        run(sqlite3.connect(dbpath, timeout=5))
    finally:
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


if __name__ == "__main__":
    main()
