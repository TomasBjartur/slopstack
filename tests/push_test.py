#!/usr/bin/env python3
"""Pushed changes (long polling) against build/server with worker
processes: a request that waits for others' changes is answered when they
come (whichever worker takes them), not with the writer's own, after its
time with nothing, and never past a change of rights; waiting is limited
per user; a client that goes away while waiting harms nothing. The same
for live comments on a post page. usage: tests/push_test.py"""
import os, socket, sqlite3, struct, subprocess, sys, tempfile, threading, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sync_test as S

PORT = 8098
WAIT_MS = 2000
S.PORT = PORT
check = S.check


def waiting(fn):
    """Runs fn in a thread: (result holder, thread, start time)."""
    box = {}

    def go():
        box["r"] = fn()
        box["t"] = time.time()

    th = threading.Thread(target=go, daemon=True)
    t0 = time.time()
    th.start()
    return box, th, t0


def wait_sync(sid, post, since, rep):
    st, body, _ = S.http("POST", f"/edit/{post}/sync?wait=1&me={rep}", sid, struct.pack("<QI", since, rep), "application/octet-stream")
    if st != 200:
        return st, None
    return st, {"seq": struct.unpack("<Q", body[1:9])[0], "ops": body[10:]}


def run(db):
    a_id, a = S.user(db, "ann@example.com", "Ann")
    b_id, b = S.user(db, "ben@example.com", "Ben")
    _, c = S.user(db, "cyd@example.com", "Cyd")
    S.http("POST", "/blogs", a, S.form({"slug": "ann", "title": "Ann"}))
    S.http("POST", "/dash/ann/authors", a, S.form({"email": "ben@example.com"}))
    _, _, loc = S.http("POST", "/dash/ann/posts", a, S.form({"title": "Shared"}))
    post = int(loc.rsplit("/", 1)[1])
    _, ra, _ = S.rep_of(a, post)
    _, rb, _ = S.rep_of(b, post)
    st, r = S.sync(a, post, 0, ra, S.ins(ra, 1, 0, 0, 1, "hello"))
    seq = r["seq"]

    # A wait with nothing new: held; answered when the co-author writes,
    # with what they wrote. Twenty times: the workers are several, and the
    # wait and the write may land on any of them.
    lat, bad = [], []
    ctr = 1
    for k in range(20):
        box, th, t0 = waiting(lambda: wait_sync(a, post, seq, ra))
        time.sleep(0.3)
        held = "r" not in box
        op = S.ins(rb, ctr, 0, 0, 1, f"b{k} ")
        ctr += len(f"b{k} ")
        tw = time.time()
        st, r = S.sync(b, post, seq, rb, op)
        th.join(5)
        got = box.get("r")
        if not (held and got and got[0] == 200 and got[1]["ops"] == op):
            bad.append((k, held, got))
        else:
            lat.append((box["t"] - tw) * 1000)
            seq = got[1]["seq"]
    lat.sort()
    check(f"a waiting editor gets a co-author's change when it is made, across 3 workers (20 of 20; median {lat[len(lat) // 2] if lat else 0:.0f} ms, worst {lat[-1] if lat else 0:.0f} ms)",
          not bad and lat and lat[-1] < 300, bad[:2])

    # Its own changes do not wake it (it has them); a co-author's then do,
    # and only theirs come.
    box, th, t0 = waiting(lambda: wait_sync(a, post, seq, ra))
    time.sleep(0.2)
    own = S.ins(ra, 6, ra, 5, 1, "!")
    S.sync(a, post, seq, ra, own)
    time.sleep(0.5)
    check("its own change does not answer the wait", "r" not in box)
    theirs = S.ins(rb, ctr, 0, 0, 1, "x")
    ctr += 1
    S.sync(b, post, seq, rb, theirs)
    th.join(5)
    got = box.get("r")
    check("…a co-author's does, with only theirs", got and got[0] == 200 and got[1]["ops"] == theirs, got)
    seq = got[1]["seq"] if got else seq

    # Nothing happens: answered at the end of its time, empty.
    box, th, t0 = waiting(lambda: wait_sync(a, post, seq, ra))
    th.join(10)
    took = (box.get("t", t0) - t0) * 1000
    got = box.get("r")
    check(f"with nothing new, answered after its time, empty ({took:.0f} ms, BLOG_WAIT_MS {WAIT_MS})",
          got and got[0] == 200 and got[1]["ops"] == b"" and WAIT_MS - 100 <= took < WAIT_MS + 1500, (got, took))

    # A request carrying changes, or with news waiting, is never held.
    t0 = time.time()
    st, _, _ = S.http("POST", f"/edit/{post}/sync?wait=1&me={ra}", a, struct.pack("<QI", 0, ra), "application/octet-stream")
    check("with something new for it: answered at once", st == 200 and time.time() - t0 < 0.5, (st, time.time() - t0))
    t0 = time.time()
    st, _, _ = S.http("POST", f"/edit/{post}/sync?wait=1&me={ra}", a, struct.pack("<QI", seq, ra) + S.ins(ra, 7, ra, 6, 1, "?"), "application/octet-stream")
    check("carrying changes: answered at once", st == 200 and time.time() - t0 < 0.5, (st, time.time() - t0))
    seq = int(db.execute("SELECT max(seq) FROM doc_ops").fetchone()[0])

    # An outsider cannot wait on the post.
    st, _ = wait_sync(c, post, seq, ra)
    check("an outsider's wait is refused (403), not held", st == 403, st)

    # Rights change while waiting: removed as an author, then the post
    # changes: the answer is a refusal, not the text.
    box, th, t0 = waiting(lambda: wait_sync(b, post, seq, rb))
    time.sleep(0.3)
    S.http("POST", f"/dash/ann/authors/{b_id}/remove", a, b"")
    S.sync(a, post, seq, ra, S.ins(ra, 8, ra, 7, 1, "secret"))
    th.join(5)
    got = box.get("r")
    check("removed while waiting: the next change answers 403, not the text", got and got[0] == 403, got)
    seq = int(db.execute("SELECT max(seq) FROM doc_ops").fetchone()[0])

    # Waiting is limited per user (per worker: 32); past it, answered at
    # once (the editor then pauses before asking again).
    boxes = [waiting(lambda: wait_sync(a, post, seq, ra)) for _ in range(150)]
    time.sleep(1.0)
    quick = sum(1 for bx, _, _ in boxes if "r" in bx)
    check(f"one user's waits are limited: of 150 at once, {quick} answered at once (at least 150 - 3 x 32)", quick >= 150 - 3 * 32, quick)
    for _, th, _ in boxes:
        th.join(10)

    # Clients that go away while waiting: nothing breaks.
    socks = []
    for _ in range(50):
        s = socket.create_connection(("127.0.0.1", PORT), timeout=5)
        body = struct.pack("<QI", seq, ra)
        s.sendall(f"POST /edit/{post}/sync?wait=1&me={ra} HTTP/1.1\r\nHost: x\r\nCookie: sid={a}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/octet-stream\r\nContent-Length: {len(body)}\r\n\r\n".encode() + body)
        socks.append(s)
    time.sleep(0.3)
    for s in socks:
        s.close()
    S.sync(a, post, seq, ra, S.ins(ra, 14, ra, 13, 1, "."))
    time.sleep(0.3)
    st, r = S.sync(a, post, 0, ra)
    check("50 waits whose clients left, then a change: the server carries on", st == 200 and r is not None, st)

    # LIVE COMMENTS: a published post's live request waits for a comment.
    S.http("POST", f"/edit/{post}", a, S.form({"title": "Shared", "action": "publish"}))
    after = int(db.execute("SELECT coalesce(max(id), 0) FROM comment").fetchone()[0])
    box, th, t0 = waiting(lambda: S.http("GET", f"/live/{post}?n=1&after={after}", None, site=None))
    time.sleep(0.4)
    check("a reader's live request waits while no one comments", "r" not in box)
    tw = time.time()
    st, _, _ = S.http("POST", f"/comment/{post}", a, S.form({"body": "Pushed comment", "parent": "0", "after": after}))
    th.join(5)
    got = box.get("r")
    took = (box.get("t", tw) - tw) * 1000
    check(f"…a comment answers it with the comment and the next request ({took:.0f} ms)",
          got and got[0] == 200 and b"Pushed comment" in got[1] and b"data-init__delay.100ms" in got[1] and took < 300, (got and got[1][:300], took))
    cid = int(db.execute("SELECT max(id) FROM comment").fetchone()[0])
    box, th, t0 = waiting(lambda: S.http("GET", f"/live/{post}?n=2&after={cid}", None, site=None))
    th.join(10)
    got = box.get("r")
    took = (box.get("t", t0) - t0) * 1000
    check(f"…with no comment, answered after its time, with the next request ({took:.0f} ms)",
          got and got[0] == 200 and b"Pushed comment" not in got[1] and b"data-init__delay.100ms" in got[1] and took >= WAIT_MS - 100, (got and got[1][:200], took))
    # Unpublished while a reader waits: the answer is 404, not comments.
    box, th, t0 = waiting(lambda: S.http("GET", f"/live/{post}?n=3&after={cid}", None, site=None))
    time.sleep(0.3)
    db.execute("UPDATE post SET published = 0 WHERE id = ?", (post,))
    db.commit()
    S.http("POST", f"/comment/{post}", a, S.form({"body": "After unpublishing", "parent": "0", "after": cid}))
    th.join(10)
    got = box.get("r")
    check("unpublished while a reader waits: 404, not the comments", got and got[0] == 404 and b"After unpublishing" not in got[1], got and got[0])


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_WORKERS="3", BLOG_WAIT_MS=str(WAIT_MS))
    srv = subprocess.Popen([os.path.join(S.ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
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
    print(f"\n{S.fails} failure(s)")
    sys.exit(1 if S.fails else 0)


main()
