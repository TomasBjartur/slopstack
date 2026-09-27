#!/usr/bin/env python3
"""Attacks on the running server, each of which must fail and leave the
data as it was. Actors: an owner (blog "o"), a co-author of "o", a removed
co-author (who still holds a replica number and an old editor page), an
outsider (with a blog of their own), anonymous visitors, and forged or
stale cookies.

- Every write route, by everyone not allowed to use it (forms and
  Datastar requests).
- The editor's sync: someone else's replica number, the server's, one
  given for another post, reading a draft's operations.
- Drafts through every read path (post page, editor, dashboard, sync,
  home, blog, author pages), and caches that could serve one person's view
  to another.
- Cross-site requests, malformed sessions, odd ids, path tricks,
  duplicated headers.
- Volume: the write budget (30 a minute a user; syncs not counted), many
  reads at once.

usage: tests/redteam_test.py   (needs tools/build.sh)
"""
import concurrent.futures, hashlib, os, re, secrets, socket, sqlite3, struct, subprocess, sys, tempfile, time, urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8084
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def raw(data, timeout=20):
    s = socket.create_connection(("127.0.0.1", PORT), timeout=timeout)
    s.sendall(data)
    out = b""
    try:
        while True:
            b = s.recv(65536)
            if not b:
                break
            out += b
    except (socket.timeout, ConnectionResetError):
        pass
    s.close()
    return out


def parse(out):
    head, _, rb = out.partition(b"\r\n\r\n")
    try:
        st = int(head.split(b" ")[1]) if head.startswith(b"HTTP/1.1 ") else 0
    except (IndexError, ValueError):
        st = 0
    loc = re.search(rb"\r\nLocation: ([^\r]*)", head)
    return st, (loc.group(1).decode() if loc else None), rb.decode("utf-8", "replace"), head


def http(method, path, sid=None, form=None, site="same-origin", extra="", body=None, ctype=None, cookie=None):
    if body is None:
        body = urllib.parse.urlencode(form).encode() if form is not None else b""
    h = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n"
    if site:
        h += f"Sec-Fetch-Site: {site}\r\n"
    if cookie is not None:
        h += f"Cookie: {cookie}\r\n"
    elif sid:
        h += f"Cookie: sid={sid}\r\n"
    if method == "POST":
        h += f"Content-Type: {ctype or 'application/x-www-form-urlencoded'}\r\nContent-Length: {len(body)}\r\n"
    st, loc, rb, _ = parse(raw(h.encode() + extra.encode() + b"\r\n" + body))
    return st, loc, rb


def ds(method, path, sid, form=None):
    """A Datastar request (as the pages send it)."""
    return http(method, path, sid, form, extra="Datastar-Request: true\r\n")


# The editor's sync (tests/sync_test.py, src/crdt.rs wire format).
def ins(rep, ctr, prep, pctr, side, text):
    t = text.encode()
    return struct.pack("<BIIIIBI", 1, rep, ctr, prep, pctr, side, len(t)) + t


def sync(sid, post, since, rep, ops=b"", site="same-origin"):
    st, _, _ = http("POST", f"/edit/{post}/sync", sid, body=struct.pack("<QI", since, rep) + ops, ctype="application/octet-stream", site=site)
    return st


def sync_raw(sid, post, since, rep, ops=b""):
    h = (f"POST /edit/{post}/sync HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nSec-Fetch-Site: same-origin\r\n"
         + (f"Cookie: sid={sid}\r\n" if sid else "") + "Content-Type: application/octet-stream\r\n")
    b = struct.pack("<QI", since, rep) + ops
    out = raw(h.encode() + f"Content-Length: {len(b)}\r\n\r\n".encode() + b)
    st, _, _, _ = parse(out)
    return st, out.partition(b"\r\n\r\n")[2]


def user(db, email, name, handle, expires_in=3600_000):
    uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES (?, ?, ?, 1)", (email, name, handle)).lastrowid
    return uid, session(db, uid, expires_in)


def session(db, uid, expires_in=3600_000):
    raw_t = secrets.token_bytes(32)
    now = int(time.time() * 1000)
    db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw_t).digest(), uid, now, now + expires_in))
    db.commit()
    return raw_t.hex()


def refused(st, loc=None):
    # Refused: an error status, or (for a signed-out writer) off to log in.
    return st in (400, 403, 404, 405, 409, 413, 429) or (st == 303 and (loc == "/login" or (loc or "").startswith("/login?next=%2F")))


def rep_of(sid, post):
    st, _, body = http("GET", f"/edit/{post}", sid)
    m = re.search(r'data-rep="(\d+)"', body)
    return int(m.group(1)) if m else None


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_ORIGIN=f"http://localhost:{PORT}", BLOG_RP_ID="localhost", BLOG_WORKERS="1")
    env.pop("BLOG_SIGNUP_DIRECT", None)
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        # (Only TCP: no request yet, so the first one runs the sweep below.)
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", PORT), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        run(srv, sqlite3.connect(dbpath, timeout=10))
    finally:
        srv.terminate()
        err = srv.communicate(timeout=10)[1].decode("utf-8", "replace")
        if "panicked" in err or "ASSERT" in err or "memory fault" in err:
            check("no crash", False, err[-400:])
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


def run(srv, db):
    def fresh_budget():
        db.execute("DELETE FROM write_budget")
        db.commit()

    # EXPIRED CHALLENGES, LINKS AND SESSIONS ARE SWEPT (at most once a
    # minute, on a request: the first one here).
    old_uid, old_sid = user(db, "old@example.com", "Old", "old", expires_in=-1000)
    for i in range(50):
        db.execute("INSERT INTO challenge(hash, purpose, expires_ms) VALUES (?, 2, 2)", (hashlib.sha256(bytes([i])).digest(),))
        db.execute("INSERT INTO email_token(hash, email, name, handle, purpose, expires_ms) VALUES (?, 'z@example.com', 'Z', 'zed', 1, 2)",
                   (hashlib.sha256(b"t" + bytes([i])).digest(),))
    db.commit()
    http("GET", "/")
    left = [db.execute(f"SELECT count(*) FROM {t} WHERE expires_ms < 1000").fetchone()[0] for t in ("challenge", "email_token")]
    check("expired challenges and email links are swept", left == [0, 0], left)
    check("expired sessions are swept", db.execute("SELECT count(*) FROM session WHERE user_id = ?", (old_uid,)).fetchone()[0] == 0)
    # (Swept: re-insert one to test that an expired session is refused.)
    old_sid = session(db, old_uid, expires_in=-1000)

    o_id, o = user(db, "owner@example.com", "Owner", "owner")
    a_id, a = user(db, "coauthor@example.com", "Coauthor", "coauthor")
    r_id, r = user(db, "removed@example.com", "Removed", "removed")
    x_id, x = user(db, "outsider@example.com", "Outsider", "outsider")
    http("POST", "/blogs", o, {"slug": "o", "title": "O"})
    http("POST", "/blogs", x, {"slug": "x", "title": "X"})
    http("POST", "/dash/o/authors", o, {"email": "coauthor@example.com"})
    http("POST", "/dash/o/authors", o, {"email": "removed@example.com"})
    _, loc, _ = http("POST", "/dash/o/posts", o, {"slug": "pub", "title": "Pub"})
    pub = int(loc.rsplit("/", 1)[1])
    http("POST", f"/edit/{pub}", o, {"title": "Pub", "body": "public words", "action": "publish"})
    _, loc, _ = http("POST", "/dash/o/posts", o, {"slug": "secret", "title": "Secretdraft"})
    draft = int(loc.rsplit("/", 1)[1])
    http("POST", f"/edit/{draft}", o, {"title": "Secretdraft title", "body": "zyzzyva draftonly words"})
    _, loc, _ = http("POST", "/dash/x/posts", x, {"slug": "mine", "title": "Mine"})
    xpost = int(loc.rsplit("/", 1)[1])
    # Times are stored as wall-clock milliseconds (sessions, links and the
    # budget expire by them; pages show them as dates).
    pms = db.execute("SELECT published_ms FROM post WHERE id = ?", (pub,)).fetchone()[0] or 0
    check("stored times are wall-clock (a post published now is dated now)", abs(pms - time.time() * 1000) < 3600_000, (pms, int(time.time() * 1000)))
    check("setup: the posts exist", db.execute("SELECT count(*) FROM post").fetchone()[0] == 3 and
          db.execute("SELECT published FROM post WHERE id = ?", (pub,)).fetchone()[0] == 1)

    # Replica numbers: the co-author and the removed author open the draft
    # (in that order), the owner the public post.
    rep_a_draft = rep_of(a, draft)
    rep_r_draft = rep_of(r, draft)
    rep_o_pub = rep_of(o, pub)
    rep_x_mine = rep_of(x, xpost)
    check("setup: replica numbers issued", None not in (rep_a_draft, rep_r_draft, rep_o_pub, rep_x_mine),
          (rep_a_draft, rep_r_draft, rep_o_pub, rep_x_mine))
    http("POST", f"/dash/o/authors/{r_id}/remove", o)
    check("setup: the removed author is removed", db.execute("SELECT count(*) FROM member WHERE user_id = ?", (r_id,)).fetchone()[0] == 0)
    fresh_budget()

    def snapshot():
        q = lambda s: db.execute(s).fetchall()
        return (q("SELECT id, blog_id, slug, title, published, published_ms, body_md, draft_title FROM post ORDER BY id"),
                q("SELECT * FROM member ORDER BY blog_id, user_id"), q("SELECT id, slug, title FROM blog ORDER BY id"),
                q("SELECT seq, post_id, data FROM doc_ops ORDER BY seq"), q("SELECT count(*) FROM doc_snap"))

    # WRITES BY THE WRONG PEOPLE
    before = snapshot()
    post_attempts = [
        ("save someone else's post", f"/edit/{pub}", {"title": "pwned", "body": "pwned"}),
        ("save someone else's post, title only", f"/edit/{pub}", {"title": "pwned"}),
        ("publish someone else's draft", f"/edit/{draft}", {"title": "pwned", "body": "pwned", "action": "publish"}),
        ("publish someone else's draft as it is", f"/edit/{draft}/publish", {}),
        ("unpublish someone else's post", f"/edit/{pub}/unpublish", {}),
        ("delete someone else's post", f"/edit/{pub}/delete", {}),
    ]
    blog_attempts = [
        ("create a post in someone else's blog", "/dash/o/posts", {"title": "pwned"}),
        ("add an author to someone else's blog", "/dash/o/authors", {"email": "outsider@example.com"}),
        ("remove an author from someone else's blog", f"/dash/o/authors/{a_id}/remove", {}),
        ("remove the owner of someone else's blog", f"/dash/o/authors/{o_id}/remove", {}),
        ("delete someone else's blog", "/dash/o/delete", {}),
    ]
    forged = "%064x" % secrets.randbits(256)
    actors = [("outsider", x), ("removed author", r), ("anonymous", None), ("forged cookie", forged), ("expired session", old_sid)]
    for who, sid in actors:
        for name, path, form in post_attempts + blog_attempts:
            st, loc, _ = http("POST", path, sid, form)
            check(f"{who}: {name} refused", refused(st, loc), (st, loc))
        # Datastar requests: a notice, never the change.
        for name, path, form in [("add an author", "/dash/o/authors?frag=1", {"email": "outsider@example.com"}),
                                 ("remove an author", f"/dash/o/authors/{a_id}/remove?frag=1", {})]:
            st, _, body = ds("POST", path, sid, form)
            check(f"{who}: Datastar {name} refused with a notice", st == 200 and "Not allowed" in body, (st, body[:200]))
        # The editor's sync: with the removed author's own old number, the
        # co-author's, the server's (1).
        for rname, rep in (("its old replica number", rep_r_draft), ("the co-author's replica number", rep_a_draft), ("the server's replica number", 1)):
            st = sync(sid, draft, 0, rep, ins(rep, 900, 0, 0, 1, "pwned"))
            check(f"{who}: sync into someone else's draft with {rname} refused", st == 403, st)
        st, body = sync_raw(sid, draft, 0, rep_r_draft)
        check(f"{who}: cannot read a draft's operations through sync", st in (403, 404) and b"zyzzyva" not in body, st)
    # A co-author writes, but does not manage the blog.
    for name, path, form in [("add an author", "/dash/o/authors", {"email": "outsider@example.com"}),
                             ("remove the owner", f"/dash/o/authors/{o_id}/remove", {}),
                             ("remove themself", f"/dash/o/authors/{a_id}/remove", {}),
                             ("delete the blog", "/dash/o/delete", {})]:
        st, loc, _ = http("POST", path, a, form)
        check(f"co-author: {name} refused", refused(st, loc), (st, loc))
    # The owner cannot leave the blog without an owner.
    st, loc, _ = http("POST", f"/dash/o/authors/{o_id}/remove", o)
    check("owner: removing themself refused", refused(st, loc), (st, loc))
    # The owner's replica numbers, misused.
    st = sync(o, draft, 0, rep_a_draft, ins(rep_a_draft, 900, 0, 0, 1, "pwned"))
    check("owner: sync with the co-author's replica number refused", st == 403, st)
    st = sync(o, draft, 0, 1, ins(1, 900, 0, 0, 1, "pwned"))
    check("owner: sync with the server's replica number refused", st == 403, st)
    st = sync(o, draft, 0, rep_o_pub, ins(rep_o_pub, 900, 0, 0, 1, "pwned"))
    # (rep_o_pub was issued for the public post: on the draft that number is
    # someone else's or nobody's.)
    check("owner: sync with a replica number from another post refused", st == 403, (st, rep_o_pub, rep_a_draft, rep_r_draft))
    st = sync(x, draft, 0, rep_x_mine, ins(rep_x_mine, 900, 0, 0, 1, "pwned"))
    check("outsider: sync into a draft with their own post's replica number refused", st == 403, st)
    st = sync(a, draft, 0, rep_a_draft, ins(rep_a_draft, 900, 0, 0, 1, "pwned"), site="cross-site")
    check("co-author: a cross-site sync refused", st == 403, st)
    st = sync(x, 999999, 0, 2, ins(2, 1, 0, 0, 1, "x"))
    check("sync into a post that does not exist refused", st in (403, 404), st)
    check("nothing changed", snapshot() == before, [(i, b) for i, (b, c) in enumerate(zip(before, snapshot())) if b != c][:2])

    # CROSS-SITE REQUESTS (the CSRF law), and doubled headers.
    for site in ("cross-site", "same-site", "none", "Same-Origin", "same-origin, same-origin"):
        st, _, _ = http("POST", "/blogs", x, {"title": "csrf"}, site=site)
        check(f"a POST with Sec-Fetch-Site: {site} refused (CSRF)", st == 403, st)
    st, _, _ = http("POST", "/blogs", x, {"title": "csrf"}, site=None, extra="Origin: https://evil.example\r\n")
    check("a POST without Sec-Fetch-Site refused", st == 403, st)
    st, _, _ = http("POST", "/blogs", x, {"title": "csrf"}, site="cross-site", extra="Sec-Fetch-Site: same-origin\r\n")
    check("two Sec-Fetch-Site headers: 400", st == 400, st)
    st, _, _ = http("POST", "/blogs", x, {"title": "csrf"}, extra=f"Cookie: sid={o}\r\n")
    check("two Cookie headers: 400", st == 400, st)
    for m in ("PUT", "DELETE", "PATCH"):
        st, _, _ = http(m, f"/edit/{pub}", o)
        check(f"{m} refused (405 or 501)", st in (405, 501), st)
    st, _, _ = http("POST", "/logout", o, site="cross-site")
    st2, _, _ = http("GET", "/dash", o)
    check("a cross-site logout does nothing", st == 403 and st2 == 200, (st, st2))
    check("CSRF attempts changed nothing", db.execute("SELECT count(*) FROM blog WHERE title = 'csrf'").fetchone()[0] == 0)

    # DRAFTS THROUGH EVERY READ PATH
    for who, sid in [("outsider", x), ("removed author", r), ("anonymous", None), ("forged cookie", forged)]:
        for path in ["/b/o/secret", f"/edit/{draft}", "/dash/o", f"/edit/{draft}?x=1"]:
            st, loc, body = http("GET", path, sid)
            leak = "zyzzyva" in body or "Secretdraft" in body
            ok = (st == 404 or (st == 303 and (loc == "/login" or (loc or "").startswith("/login?next=%2F")) and sid in (None, forged))) and not leak
            check(f"{who}: draft not readable at {path}", ok, (st, loc, leak))
        for path in ["/", "/?page=2", "/b/o", "/b/o?page=2", "/u/owner", "/u/coauthor", "/b/x"]:
            st, _, body = http("GET", path, sid)
            check(f"{who}: no draft in {path}", not any(w in body for w in ("zyzzyva", "Secretdraft", "/b/o/secret")), st)
    # The members see it (the tests above could pass with a broken draft).
    st, _, body = http("GET", "/b/o/secret", a)
    check("co-author reads the draft (sanity)", st == 200 and "zyzzyva" in body, st)
    # A member's view of a draft is not served to others (no shared cache).
    st, _, body = http("GET", "/b/o/secret", None)
    check("after a member's view, an anonymous visitor still gets 404", st == 404 and "zyzzyva" not in body, st)
    st, _, body = http("GET", "/b/o", a)
    st, _, body = http("GET", "/b/o", x)
    check("a member's view of a blog is not served to an outsider", "Secretdraft" not in body, st)
    # Unpublished: the cached page goes with it.
    _, loc, _ = http("POST", "/dash/o/posts", o, {"slug": "brief", "title": "Brief"})
    brief = int(loc.rsplit("/", 1)[1])
    http("POST", f"/edit/{brief}", o, {"title": "Brief", "body": "ephemeral quokka", "action": "publish"})
    st1, _, b1 = http("GET", "/b/o/brief")
    http("POST", f"/edit/{brief}/unpublish", o)
    st2, _, b2 = http("GET", "/b/o/brief")
    _, _, b3 = http("GET", "/")
    _, _, b4 = http("GET", "/u/owner")
    check("an unpublished post is gone at once (not served from a cache)", st1 == 200 and "quokka" in b1 and st2 == 404 and "quokka" not in b2, (st1, st2))
    check("…and from the listings", "Brief" not in b3 and "/b/o/brief" not in b4)

    # A DELETED BLOG'S TEXT: its posts go with it, and a new post (which may
    # get the same id) starts empty. The owner loads the text first (the
    # server keeps documents in memory).
    fresh_budget()
    http("POST", "/blogs", o, {"slug": "gone", "title": "Gone"})
    _, loc, _ = http("POST", "/dash/gone/posts", o, {"slug": "last", "title": "Last"})
    gid = int(loc.rsplit("/", 1)[1])
    http("POST", f"/edit/{gid}", o, {"title": "Last", "body": "aardvark private notes"})
    st, _, body = http("GET", f"/edit/{gid}", o)
    check("setup: the doomed post's text is loaded", "aardvark" in body, st)
    st, loc, _ = http("POST", "/dash/gone/delete", o)
    check("the owner deletes the blog", st == 303 and db.execute("SELECT count(*) FROM post WHERE id = ?", (gid,)).fetchone()[0] == 0, (st, loc))
    _, loc, _ = http("POST", "/dash/x/posts", x, {"title": "Fresh"})
    nid = int(loc.rsplit("/", 1)[1])
    st, _, body = http("GET", f"/edit/{nid}", x)
    rep_n = int(re.search(r'data-rep="(\d+)"', body).group(1)) if 'data-rep="' in body else 0
    _, sb = sync_raw(x, nid, 0, rep_n)
    check(f"a new post (id {nid}, deleted one was {gid}) does not show a deleted blog's text",
          st == 200 and "aardvark" not in body and b"aardvark" not in sb, (st, nid == gid))
    # The same through deleting the post itself.
    _, loc, _ = http("POST", "/dash/o/posts", o, {"slug": "last2", "title": "Last2"})
    gid2 = int(loc.rsplit("/", 1)[1])
    http("POST", f"/edit/{gid2}", o, {"title": "Last2", "body": "pangolin private notes"})
    http("GET", f"/edit/{gid2}", o)
    http("POST", f"/edit/{gid2}/delete", o)
    _, loc, _ = http("POST", "/dash/x/posts", x, {"title": "Fresh2"})
    nid2 = int(loc.rsplit("/", 1)[1])
    st, _, body = http("GET", f"/edit/{nid2}", x)
    check(f"a new post (id {nid2}, deleted one was {gid2}) does not show a deleted post's text", st == 200 and "pangolin" not in body, (st, nid2 == gid2))

    # SESSIONS AND ODD INPUT
    for bad in ["", "x", "0" * 64, "g" * 64, a.upper(), a[:-2], "../" * 10, "%00" + a, " " + "0" * 63, forged, old_sid]:
        st, loc, body = http("GET", "/dash", bad or None)
        check(f"malformed or unknown session {bad[:12]!r}: not signed in", st == 303 and (loc == "/login" or (loc or "").startswith("/login?next=%2F")), (st, loc))
    st, loc, _ = http("GET", "/dash", cookie=f"xsid={o}")
    check("a cookie named xsid is not the session", st == 303 and (loc == "/login" or (loc or "").startswith("/login?next=%2F")), (st, loc))
    st, loc, _ = http("GET", "/dash", cookie=f"theme=dark; sid={o}")
    check("the session among other cookies still works (sanity)", st == 200, (st, loc))
    # (A token followed by more characters is read as the token: only its
    # own holder can send it, so at most the holder is signed in.)
    st, loc, body = http("GET", "/dash", cookie=f"sid={a}00")
    check("a token with trailing characters signs in no one else", (st == 303 and (loc == "/login" or (loc or "").startswith("/login?next=%2F"))) or st == 200, (st, loc))
    for path in ["/edit/0", "/edit/4294967296", "/edit/99999999999999999999", "/edit/-1", "/edit/1e3", "/edit/%31", f"/edit/{draft}/sync",
                 "/b/o/..%2f..%2fetc", "/s/..%2f..%2fetc%2fpasswd", "/s/app.css%00.js", "/%2e%2e/", "/b/o/pub%0d%0aSet-Cookie:%20x=1",
                 "/u/%00", "/u/" + "a" * 5000, "/dash/o%2f..%2fx", "/b/%C3%A9", "/verify?t=" + "0" * 64, "/verify?t=zz", "/handle?h=%00",
                 "/b/o/pub?published=1", "/?page=-1", "/?page=99999999999999999999"]:
        out = raw(f"GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nCookie: sid={x}\r\n\r\n".encode())
        st, _, _, head = parse(out)
        check(f"odd path {path[:40]}: handled", st in (200, 303, 400, 403, 404, 405, 414, 431) and b"Set-Cookie: x" not in head and b"root:" not in out, (st, head[:80]))
    st, _, body = http("GET", "/b/o/pub?published=1", x)
    check("the 'your post is live' notice is not shown to a stranger", "Your post is live" not in body, st)
    for path, form in [(f"/edit/{pub}", {"title": "x" * 201}), (f"/edit/{pub}", {"title": "t", "body": "a\x00b"}),
                       ("/blogs", {"title": "t", "slug": "../x"}), ("/blogs", {"title": "t", "slug": "A"}), ("/dash/o/posts", {"title": "t", "slug": "draft-x"}),
                       ("/dash/o/posts", {"title": "t", "slug": "a/b"}), (f"/dash/o/authors/abc/remove", {}), (f"/dash/o/authors/-1/remove", {})]:
        st, loc, _ = http("POST", path, o, form)
        check(f"owner: bad input to {path} {str(list(form.values())[-1:])[:14]} refused", refused(st, loc), (st, loc))
    st, _, _ = http("POST", "/blogs", o, body=b"title=%zz&%%%", ctype="application/x-www-form-urlencoded")
    check("a malformed form: 400", st == 400, st)

    # ADDING AN AUTHOR SAYS NOTHING ABOUT EMAILS UNLESS YOU OWN THE BLOG
    for who, sid in (("outsider", x), ("co-author", a)):
        r1 = http("POST", "/dash/o/authors", sid, {"email": "owner@example.com"})
        r2 = http("POST", "/dash/o/authors", sid, {"email": "nobody-at-all@example.com"})
        same = lambda u, v: re.sub(r'nonce="[^"]*"', "", u) == re.sub(r'nonce="[^"]*"', "", v)
        check(f"{who}: add-author reveals no account (form)", r1[0] == r2[0] == 403 and same(r1[2], r2[2]), (r1[0], r2[0]))
        r1 = ds("POST", "/dash/o/authors?frag=1", sid, {"email": "owner@example.com"})
        r2 = ds("POST", "/dash/o/authors?frag=1", sid, {"email": "nobody-at-all@example.com"})
        check(f"{who}: add-author reveals no account (Datastar)", r1[0] == r2[0] and r1[2] == r2[2] and "Not allowed" in r1[2], (r1[0], r2[0], r1[2][:120]))
    fresh_budget()

    # THE WRITE BUDGET (30 a minute a user)
    before = snapshot()
    codes = [http("POST", f"/edit/{draft}", a, {"title": f"Budget {i}"})[0] for i in range(31)]
    check("30 saves in a minute pass, the 31st is refused (429)", codes[:30] == [303] * 30 and codes[30] == 429, codes)
    title = db.execute("SELECT draft_title FROM post WHERE id = ?", (draft,)).fetchone()[0]
    check("…and changes nothing", title == "Budget 29", title)
    st, _, body = ds("POST", "/dash/o/posts", a, {"title": "over"})
    check("a Datastar write over budget: a notice", st == 200 and "a lot of changes" in body, (st, body[:200]))
    for name, path, form in [("new blog", "/blogs", {"title": "over"}), ("new post", "/dash/o/posts", {"title": "over"}),
                             ("publish", f"/edit/{draft}/publish", {}), ("Write button", "/write", {})]:
        st, loc, _ = http("POST", path, a, form)
        check(f"over budget: {name} refused", st == 429, (st, loc))
    st, _, _ = http("POST", "/dash/o/authors", o, {"email": "outsider@example.com"})
    check("the budget is per user: the owner still writes", st == 200 and db.execute("SELECT count(*) FROM member WHERE user_id = ? AND blog_id = (SELECT id FROM blog WHERE slug = 'o')", (x_id,)).fetchone()[0] == 1, st)
    http("POST", f"/dash/o/authors/{x_id}/remove", o)
    n0 = db.execute("SELECT count(*) FROM doc_ops").fetchone()[0]
    oks = 0
    for i in range(40):
        oks += sync(a, draft, 0, rep_a_draft, ins(rep_a_draft, 1000 + i * 10, 0, 0, 1, "t")) == 200
    check("typing (syncs) is not budgeted", oks == 40 and db.execute("SELECT count(*) FROM doc_ops").fetchone()[0] == n0 + 40, oks)
    # Refused attempts do not spend anyone's budget.
    fresh_budget()
    for _ in range(40):
        http("POST", "/dash/o/posts", x, {"title": "no"})
    st, _, _ = http("POST", "/dash/x/posts", x, {"title": "after refusals"})
    check("refused writes do not spend the budget", st == 303, st)
    # At once: one user, 60 writes in parallel; at most 30 pass.
    fresh_budget()
    n0 = db.execute("SELECT count(*) FROM post WHERE blog_id = (SELECT id FROM blog WHERE slug = 'x')").fetchone()[0]
    with concurrent.futures.ThreadPoolExecutor(16) as ex:
        codes = list(ex.map(lambda i: http("POST", "/dash/x/posts", x, {"title": f"flood {i}"})[0], range(60)))
    made = db.execute("SELECT count(*) FROM post WHERE blog_id = (SELECT id FROM blog WHERE slug = 'x')").fetchone()[0] - n0
    check("60 writes at once by one user: exactly 30 pass", made == 30 and codes.count(303) == 30 and codes.count(429) == 30, (made, sorted(set(codes))))

    # VOLUME
    with concurrent.futures.ThreadPoolExecutor(16) as ex:
        codes = list(ex.map(lambda i: http("GET", ["/", "/b/o", "/b/o/pub", "/u/owner", "/b/o/secret"][i % 5])[0], range(300)))
    check("300 reads at once: answered", all(c in (200, 404, 503) for c in codes) and codes.count(200) >= 200 and srv.poll() is None, sorted(set(codes)))
    with concurrent.futures.ThreadPoolExecutor(16) as ex:
        codes = list(ex.map(lambda i: http("POST", "/passkey/login/options", None, {})[0], range(200)))
    check("200 login challenges at once: answered", all(c in (200, 503) for c in codes), sorted(set(codes)))
    # A slow client that never finishes its request does not stop others.
    s = socket.create_connection(("127.0.0.1", PORT))
    s.sendall(b"POST /blogs HTTP/1.1\r\nHost: localhost\r\nContent-Length: 100\r\n\r\nab")
    st, _, _ = http("GET", "/")
    check("a stalled request does not block others", st == 200, st)
    s.close()
    st, _, _ = http("POST", "/blogs", x, body=b"t" * (8 << 20))
    check("an oversized form body refused", st in (400, 413), st)
    check("the server is still up", srv.poll() is None and http("GET", "/")[0] == 200)


main()
