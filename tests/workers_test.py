#!/usr/bin/env python3
"""Worker processes (BLOG_WORKERS): four workers on one port must agree
(a write through one is seen at once through every other: post pages,
feeds, documents synced through different workers), and a worker that
dies is replaced while the others keep serving.
usage: tests/workers_test.py   (SERVER=path to try another binary)"""
import hashlib, os, re, secrets, signal, socket, sqlite3, struct, subprocess, sys, tempfile, time, urllib.parse

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SERVER = os.environ.get("SERVER", os.path.join(ROOT, "build/server"))
PORT = 8088
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def http(method, path, sid=None, body=b"", ctype="application/x-www-form-urlencoded"):
    h = f"{method} {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\nSec-Fetch-Site: same-origin\r\n"
    if sid:
        h += f"Cookie: sid={sid}\r\n"
    if method == "POST":
        h += f"Content-Type: {ctype}\r\nContent-Length: {len(body)}\r\n"
    s = socket.create_connection(("127.0.0.1", PORT), timeout=10)
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
    return int(head.split(b" ")[1]), (loc.group(1).decode() if loc else None), rb


def children(pid):
    out = []
    for d in os.listdir("/proc"):
        if d.isdigit():
            try:
                stat = open(f"/proc/{d}/stat").read()
                if int(stat.rsplit(")", 1)[1].split()[1]) == pid:
                    out.append(int(d))
            except (OSError, ValueError, IndexError):
                pass
    return sorted(out)


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_WORKERS="4")
    srv = subprocess.Popen([SERVER], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    try:
        for _ in range(50):
            try:
                socket.create_connection(("127.0.0.1", PORT), timeout=0.2).close()
                break
            except OSError:
                time.sleep(0.1)
        time.sleep(0.3)
        kids = children(srv.pid)
        check("four workers", len(kids) == 4, kids)
        db = sqlite3.connect(dbpath, timeout=5)
        uid = db.execute("INSERT INTO user(email, name, handle, created_ms) VALUES ('w@x.io', 'W', 'www', 1)").lastrowid
        raw = secrets.token_bytes(32)
        now = int(time.time() * 1000)
        db.execute("INSERT INTO session VALUES (?, ?, ?, ?)", (hashlib.sha256(raw).digest(), uid, now, now + 3600_000))
        db.commit()
        sid = raw.hex()
        form = lambda d: urllib.parse.urlencode(d).encode()
        http("POST", "/blogs", sid, form({"slug": "w", "title": "W"}))
        _, loc, _ = http("POST", "/dash/w/posts", sid, form({"title": "P", "slug": "p"}))
        post = int(loc.rsplit("/", 1)[1])
        # Warm every worker's caches, then change the post and read many times.
        stale = 0
        for rev in range(5):
            db.execute("DELETE FROM write_budget")
            db.commit()
            http("POST", f"/edit/{post}", sid, form({"title": f"Rev {rev}", "body": f"body {rev}", "action": "publish"}))
            for _ in range(20):
                _, _, b = http("GET", "/b/w/p")
                if f"body {rev}".encode() not in b or f"Rev {rev}".encode() not in b:
                    stale += 1
                _, _, b = http("GET", "/")
                if f"Rev {rev}".encode() not in b:
                    stale += 1
        check("post pages and the feed: fresh through every worker (200 reads after 5 changes)", stale == 0, stale)
        # The document: batches sent through different workers (connections
        # land on any), each built on the last; every answer agrees.
        reps = []
        for _ in range(3):
            _, _, b = http("GET", f"/edit/{post}", sid)
            reps.append(int(re.search(rb'data-rep="(\d+)"', b).group(1)))
        ok = True
        last = (0, 0)  # the first a child of the root; each next one of the one before
        for i in range(60):
            r = reps[i % 3]
            ctr = 1 + i // 3
            t = b"x"
            op = struct.pack("<BIIIIBI", 1, r, ctr, last[0], last[1], 1, 1) + t
            st, _, body = http("POST", f"/edit/{post}/sync", sid, struct.pack("<QI", 0, r) + op, "application/octet-stream")
            if st != 200:
                ok = False
                print("  sync", i, st, body[:100])
                break
            last = (r, ctr)
        check("60 batches through different workers, each on the last: all stored", ok)
        http("POST", f"/edit/{post}/publish", sid)
        body_of = lambda b: b.split(b'<div class="body">', 1)[1].split(b"</div>", 1)[0]
        texts = {body_of(http("GET", "/b/w/p")[2]).count(b"x") for _ in range(12)}
        check("…and every worker publishes the same text", texts == {60}, texts)
        # A worker dies: the others keep serving, and it is replaced.
        victim = kids[0]
        os.kill(victim, signal.SIGKILL)
        served = sum(http("GET", "/")[0] == 200 for _ in range(30))
        check("with a worker killed, requests are still served", served == 30, served)
        time.sleep(1.5)
        now_kids = children(srv.pid)
        check("the dead worker is replaced", len(now_kids) == 4 and victim not in now_kids, now_kids)
        # The supervisor ends: the workers end with it.
        srv.terminate()
        srv.wait()
        time.sleep(0.5)
        alive = [k for k in now_kids if os.path.exists(f"/proc/{k}") and "Z" not in open(f"/proc/{k}/stat").read().rsplit(")", 1)[1].split()[0]]
        check("stopping the server stops every worker", not alive, alive)
    finally:
        if srv.poll() is None:
            srv.terminate()
            srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


main()
