#!/usr/bin/env python3
"""Converts the first version's database (bent-web, ~/web) into this one's.

Carried over: users (with their handles), passkeys (so everyone logs in as
before), sessions (so nobody is logged out), blogs, authors, posts (their
published text, and the editor's current text as a new document), dates.

NOT carried over (not in this version): comments, likes, images, tags,
scheduled publishing (a scheduled post becomes a draft), custom domains,
search index, mail outbox. The script prints what it leaves behind.

The editor text of each post is the old document's text, computed by the
old version's own reference CRDT (~/web/build/crdt_ref) from its
operations; it becomes one insert by the server's replica (1).

usage: tools/import_old.py OLD.db NEW.db   (NEW.db must not exist; the old
one is only read. Take OLD.db as a backup copy, not the live file.)"""
import os, re, socket, sqlite3, struct, subprocess, sys, tempfile, time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRDT_REF = os.path.expanduser("~/web/build/crdt_ref")


def new_schema(path):
    # The server migrates a new database when it starts: start it once.
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    p = subprocess.Popen([os.path.join(ROOT, "build/server")], env=dict(os.environ, PORT=str(port), BLOG_DB=path),
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        for _ in range(100):
            try:
                socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
                return
            except OSError:
                time.sleep(0.1)
        sys.exit("the server did not start")
    finally:
        p.terminate()
        p.wait()


def old_text(old, post_id):
    rows = old.execute("SELECT ctr, rep, kind, pctr, prep, side, ch FROM op WHERE post_id = ? ORDER BY seq", (post_id,)).fetchall()
    if not rows:
        return None
    with tempfile.NamedTemporaryFile("w", delete=False) as f:
        f.write("".join(f"{a}.{b}.{c}.{d}.{e}.{g}.{h};" for a, b, c, d, e, g, h in rows))
        path = f.name
    try:
        out = subprocess.run([CRDT_REF, path], capture_output=True, check=True).stdout.decode("utf-8")
    finally:
        os.unlink(path)
    m = re.search(r"<<TEXT>>(.*)<<END>>", out, re.S)
    if not m:
        sys.exit(f"post {post_id}: the old reference refused its operations")
    return m.group(1)


def words(md):
    return len(re.findall(r"\w+", md))


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    old_path, new_path = sys.argv[1], sys.argv[2]
    if os.path.exists(new_path):
        sys.exit(f"{new_path} exists")
    old = sqlite3.connect(f"file:{old_path}?mode=ro", uri=True)
    new_schema(new_path)
    new = sqlite3.connect(new_path)
    new.execute("PRAGMA foreign_keys = ON")
    with new:
        for uid, email, name, created, handle in old.execute("SELECT id, email, name, created_ms, handle FROM user"):
            h = handle or re.sub(r"[^a-z0-9_]", "", email.split("@")[0].lower())[:30]
            if len(h) < 3 or not h[0].isalpha():
                h = f"user{uid}"
            new.execute("INSERT INTO user(id, email, name, handle, created_ms) VALUES (?, ?, ?, ?, ?)", (uid, email.lower(), name, h, created))
        for cid, uid, x, y, count, created in old.execute("SELECT id, user_id, x, y, sign_count, created_ms FROM credential"):
            new.execute("INSERT INTO credential(id, user_id, public_key, sign_count, created_ms) VALUES (?, ?, ?, ?, ?)", (cid, uid, x + y, count, created))
        for row in old.execute("SELECT token_hash, user_id, created_ms, expires_ms FROM session"):
            new.execute("INSERT INTO session(token_hash, user_id, created_ms, expires_ms) VALUES (?, ?, ?, ?)", row)
        for bid, slug, title, created in old.execute("SELECT id, slug, title, created_ms FROM blog"):
            new.execute("INSERT INTO blog(id, slug, title, created_ms) VALUES (?, ?, ?, ?)", (bid, slug, title, created))
        for row in old.execute("SELECT blog_id, user_id, role FROM member"):
            new.execute("INSERT INTO member(blog_id, user_id, role) VALUES (?, ?, ?)", row)
        posts = old.execute("""SELECT p.id, p.blog_id, p.slug, p.title, p.published, p.updated_ms, p.author_id, p.published_ms,
                                      p.publish_at_ms, coalesce(b.body_md, p.body_md)
                               FROM post p LEFT JOIN post_body b ON b.post_id = p.id""").fetchall()
        scheduled = 0
        for pid, bid, slug, title, published, updated, author, pub_ms, at_ms, body in posts:
            text = old_text(old, pid)
            if text is None:
                text = body
            if at_ms and not published:
                scheduled += 1
            new.execute("""INSERT INTO post(id, blog_id, slug, title, draft_title, author_id, published, published_ms, updated_ms,
                                            words, body_md, edited_ms)
                           VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""",
                        (pid, bid, slug, title, title, author, published, pub_ms if published else pub_ms, updated,
                         words(body) if published else 0, body if published else "", updated))
            if text:
                b = text.encode("utf-8")
                op = struct.pack("<BIIIIBI", 1, 1, 1, 0, 0, 1, len(b)) + b
                new.execute("INSERT INTO doc_ops(post_id, data) VALUES (?, ?)", (pid, op))
    left = {t: old.execute(f"SELECT count(*) FROM {t}").fetchone()[0] for t in ("comment", "post_like", "image", "post_tag")}
    print(f"carried over: {len(posts)} posts, "
          f"{new.execute('SELECT count(*) FROM user').fetchone()[0]} users, "
          f"{new.execute('SELECT count(*) FROM credential').fetchone()[0]} passkeys, "
          f"{new.execute('SELECT count(*) FROM blog').fetchone()[0]} blogs")
    print("left behind (not in this version): " + ", ".join(f"{n} {t}" for t, n in left.items()) + f", {scheduled} schedules")


main()
