#!/usr/bin/env python3
"""The performance comparison: this server against the first version
(Bend + C) and its Next.js (React) baseline, on the same data, and a real
Substack post over the internet.

- Data: the first version's benchmark set (50 blogs x 40 posts, 200
  writers), made in its schema by its own binary, then converted with
  tools/import_old.py, so both versions serve the same posts. The post
  page measured has no comments (this version has none; the first version
  and Next would render them otherwise).
- Server: throughput and latency per page, server on core 0, the load
  generator (tools/loadgen, 32 connections) on core 1. Next and the first
  version as in the first version's docs/PERF.md; one worker each.
- Bytes: HTML and JavaScript per page, raw and gzipped.
- Browser: headless Chrome with 4x CPU throttling (a mid-range phone):
  TTFB, FCP, LCP, load, main-thread script time; medians of 5.
- This version only: a 6 MB novel's post page (the first version capped
  posts at 1 MiB), and the editor opening it.
- Substack: one real post: bytes, JS, and browser metrics (its TTFB
  includes the internet; ours does not: compare the rest).
Writes docs/bench.json and prints a table.
usage: tests/bench.py   (needs ~/web built: build/server, baseline/next)
       tests/bench.py --server [BINARY]   this server alone: requests a
       second, latency and CPU per request on the four pages (to compare
       two builds of it, e.g. before and after a change; a minute)"""
import gzip, json, os, re, socket, sqlite3, statistics, subprocess, sys, tempfile, time, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import start_chrome, page_ws, wait_port

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OLD = os.path.expanduser("~/web")
NODE = os.path.expanduser("~/opt/node/bin")
NEW, BEND, NEXT = 8195, 8196, 3000
SUBSTACK_HOME = "https://on.substack.com/"


def old_bench_functions():
    """The first version's data maker and measurements (tests/bench_vs_next.py,
    minus its main())."""
    src = open(os.path.join(OLD, "tests/bench_vs_next.py")).read()
    src = src[: src.rindex("\nmain()")]
    src = src.replace('ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))', f'ROOT = {OLD!r}')
    g = {"__file__": os.path.join(OLD, "tests/bench_vs_next.py"), "__name__": "old_bench"}
    sys.path.insert(0, os.path.join(OLD, "tests"))
    exec(compile(src, "old_bench", "exec"), g)
    return g


O = old_bench_functions()
get, weight, browser = O["get"], O["weight"], O["browser"]


def load(port, path, seconds=5):
    out = subprocess.run(["taskset", "-c", "1", os.path.join(ROOT, "build/loadgen"), str(port), "32", str(seconds), path],
                         capture_output=True, text=True).stdout
    m = dict(kv.split("=") for kv in out.split() if "=" in kv)
    return {"req_s": float(m["req/s"]), "p50_us": int(m["p50"][:-2]), "p99_us": int(m["p99"][:-2]), "errors": int(m["err"])}


def cpu_per_request(pid, port, path, n=2000):
    """Server CPU time per request (user + system), from /proc."""
    def cpu():
        f = open(f"/proc/{pid}/stat").read().rsplit(")", 1)[1].split()
        return (int(f[11]) + int(f[12])) / os.sysconf("SC_CLK_TCK")
    c0 = cpu()
    for _ in range(n):
        get(port, path)
    return (cpu() - c0) / n * 1e6


def substack():
    home = urllib.request.urlopen(urllib.request.Request(SUBSTACK_HOME, headers={"User-Agent": "Mozilla/5.0"}), timeout=20).read().decode("utf-8", "replace")
    m = re.search(r'href="(https://on\.substack\.com/p/[a-z0-9-]+)"', home)
    url = m.group(1) if m else SUBSTACK_HOME
    req = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0", "Accept-Encoding": "identity"})
    body = urllib.request.urlopen(req, timeout=20).read()
    scripts = [s.decode() for s in re.findall(rb'<script[^>]+src="([^"]+)"', body)]
    js = b""
    for s in scripts:
        if s.startswith("//"):
            s = "https:" + s
        if s.startswith("/"):
            s = "https://on.substack.com" + s
        try:
            js += urllib.request.urlopen(urllib.request.Request(s, headers={"User-Agent": "Mozilla/5.0", "Accept-Encoding": "identity"}), timeout=20).read()
        except Exception:
            pass
    inline = b"".join(re.findall(rb"<script(?![^>]*src=)[^>]*>(.*?)</script>", body, re.S))
    return url, {"html": len(body), "html_gz": len(gzip.compress(body)), "js_files": len(scripts),
                 "js": len(js) + len(inline), "js_gz": len(gzip.compress(js)) + len(gzip.compress(inline))}


def server_only(binary):
    """This server alone, on the same data: throughput, latency and CPU."""
    tmp = tempfile.mkdtemp()
    old_db, new_db = os.path.join(tmp, "old.db"), os.path.join(tmp, "new.db")
    O["make_db"](old_db)
    subprocess.run([sys.executable, os.path.join(ROOT, "tools/import_old.py"), old_db, new_db], check=True, stdout=subprocess.DEVNULL)
    q = sqlite3.connect(old_db)
    slug, blog = q.execute("SELECT p.slug, b.slug FROM post p JOIN blog b ON b.id = p.blog_id WHERE b.id = 7 AND p.id NOT IN (SELECT post_id FROM comment) LIMIT 1").fetchone()
    q.close()
    pages = {"home": "/", "blog": f"/b/{blog}", "post": f"/b/{blog}/{slug}", "author": "/u/writer_7"}
    proc = subprocess.Popen(["taskset", "-c", "0", binary], env=dict(os.environ, BLOG_DB=new_db, PORT=str(NEW)), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        wait_port(NEW)
        for path in pages.values():
            for _ in range(30):
                get(NEW, path)
        for page, path in pages.items():
            s_ = load(NEW, path)
            cpu = cpu_per_request(proc.pid, NEW, path)
            print(f"  {page:7} {s_['req_s']:>8.0f} req/s  p50 {s_['p50_us'] / 1000:6.2f} ms  p99 {s_['p99_us'] / 1000:6.2f} ms  cpu {cpu:6.1f} us")
    finally:
        proc.terminate()


def main():
    if "--server" in sys.argv:
        i = sys.argv.index("--server")
        return server_only(sys.argv[i + 1] if len(sys.argv) > i + 1 else os.path.join(ROOT, "build/server"))
    tmp = tempfile.mkdtemp()
    old_db = os.path.join(tmp, "old.db")
    new_db = os.path.join(tmp, "new.db")
    O["make_db"](old_db)
    subprocess.run([sys.executable, os.path.join(ROOT, "tools/import_old.py"), old_db, new_db], check=True, stdout=subprocess.DEVNULL)
    q = sqlite3.connect(old_db)
    # A post with no comments (this version has none to show).
    slug, blog = q.execute("SELECT p.slug, b.slug FROM post p JOIN blog b ON b.id = p.blog_id WHERE b.id = 7 AND p.id NOT IN (SELECT post_id FROM comment) LIMIT 1").fetchone()
    q.close()
    post = f"/b/{blog}/{slug}"
    # The novel: 6 MB of Markdown, published in blog 8 (this version only).
    n = sqlite3.connect(new_db)
    para = "The river of long evenings carries *small boats* past old walls where people talk about **books** and [maps](https://example.com). " * 8 + "\n\n"
    novel = ("# A novel\n\n" + para * (6_000_000 // len(para))).encode()
    n.execute("INSERT INTO post(blog_id, slug, title, draft_title, author_id, published, published_ms, updated_ms, words, body_md) "
              "VALUES (8, 'novel', 'A novel', 'A novel', 8, 1, 1700000000000, 1700000000000, 1000000, ?)", (novel.decode(),))
    nid = n.execute("SELECT id FROM post WHERE slug = 'novel'").fetchone()[0]
    import struct
    n.execute("INSERT INTO doc_ops(post_id, data) VALUES (?, ?)", (nid, struct.pack("<BIIIIBI", 1, 1, 1, 0, 0, 1, len(novel)) + novel))
    n.commit()
    n.close()
    pages = {"home": ("/", "/"), "blog": (f"/b/{blog}", f"/b/{blog}"), "post": (post, post), "author": ("/u/writer_7", "/u/7")}
    env = dict(os.environ, NEXT_TELEMETRY_DISABLED="1", PATH=NODE + ":" + os.environ["PATH"])
    procs = {
        "new": subprocess.Popen(["taskset", "-c", "0", os.path.join(ROOT, "build/server")], env=dict(env, BLOG_DB=new_db, PORT=str(NEW)),
                                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
        "bend": subprocess.Popen(["taskset", "-c", "0", os.path.join(OLD, "build/server")], env=dict(env, BLOG_DB=old_db, PORT=str(BEND), BLOG_WORKERS="1"),
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
        "next": subprocess.Popen(["taskset", "-c", "0", os.path.join(NODE, "node"), "node_modules/next/dist/bin/next", "start", "-p", str(NEXT)],
                                 cwd=os.path.join(OLD, "baseline/next"), env=dict(env, BLOG_DB=old_db), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL),
    }
    ports = {"new": NEW, "bend": BEND, "next": NEXT}
    os.makedirs(os.path.join(tmp, "other"), exist_ok=True)
    open(os.path.join(tmp, "other", "start.html"), "w").write("<!doctype html><title>elsewhere</title><p>another site")
    other = subprocess.Popen([sys.executable, "-m", "http.server", "8117", "--bind", "127.0.0.1", "--directory", os.path.join(tmp, "other")],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    chrome = None
    report = {"machine": "cloud container, 2 vCPU; servers on core 0, load generator on core 1", "pages": {k: v[0] for k, v in pages.items()},
              "post_note": "a post without comments"}
    try:
        for p in ports.values():
            wait_port(p)
        wait_port(8117)
        for name, port in ports.items():
            idx = 0 if name == "new" else 1
            for _, paths in pages.items():
                for _ in range(30):
                    get(port, paths[idx])
        for name, port in ports.items():
            idx = 0 if name == "new" else 1
            report[name] = {"weight": {}, "server": {}, "browser": {}}
            for page, paths in pages.items():
                report[name]["weight"][page] = weight(port, paths[idx])
                report[name]["server"][page] = load(port, paths[idx])
        report["new"]["cpu_us"] = {page: round(cpu_per_request(procs["new"].pid, NEW, paths[0]), 1) for page, paths in pages.items()}
        report["bend"]["cpu_us"] = {page: round(cpu_per_request(procs["bend"].pid, BEND, paths[1]), 1) for page, paths in pages.items()}
        # The novel: its page (rendered once, then cached), and the editor.
        report["novel"] = {"weight": weight(NEW, "/b/blog-8/novel"), "server": load(NEW, "/b/blog-8/novel")}
        chrome = start_chrome(9351, f"{tmp}/chrome")
        ws = page_ws(9351)
        ws.call("Page.enable")
        ws.call("Network.enable")
        ws.call("Emulation.setCPUThrottlingRate", {"rate": 4})
        for name, port in ports.items():
            idx = 0 if name == "new" else 1
            for page, paths in pages.items():
                report[name]["browser"][page] = browser(ws, f"http://localhost:{port}{paths[idx]}")
        report["novel"]["browser"] = browser(ws, f"http://localhost:{NEW}/b/blog-8/novel", runs=3)
        try:
            url, w = substack()
            ws.call("Emulation.setCPUThrottlingRate", {"rate": 4})
            report["substack"] = {"url": url, "weight": w, "browser": browser(ws, url, runs=3)}
        except Exception as e:
            report["substack"] = {"error": str(e)}
    finally:
        if chrome:
            chrome.terminate()
        for p in procs.values():
            p.terminate()
        other.terminate()
    json.dump(report, open(os.path.join(ROOT, "docs/bench.json"), "w"), indent=2)
    for page in pages:
        print(f"\n== {page}")
        for name in ports:
            w, s, b = report[name]["weight"][page], report[name]["server"][page], report[name]["browser"][page]
            cpu = report[name].get("cpu_us", {}).get(page)
            print(f"  {name:5} {s['req_s']:>8.0f} req/s  p50 {s['p50_us'] / 1000:6.2f} ms  p99 {s['p99_us'] / 1000:6.2f} ms  "
                  + (f"cpu {cpu:6.0f} us  " if cpu else " " * 16)
                  + f"html {w['html'] / 1024:5.1f} KB ({w['html_gz'] / 1024:4.1f} gz)  js {w['js'] / 1024:6.1f} KB ({w['js_gz'] / 1024:5.1f} gz)  "
                  f"ttfb {b['ttfb']:5.0f}  fcp {b['fcp']:5.0f}  lcp {b['lcp']:5.0f}  load {b['load']:5.0f}  script {b['script_ms']:5.0f} ms")
    nv = report["novel"]
    print(f"\n== a 6 MB novel (this version): {nv['server']['req_s']:.0f} req/s, p50 {nv['server']['p50_us'] / 1000:.1f} ms, "
          f"html {nv['weight']['html'] / 1e6:.1f} MB ({nv['weight']['html_gz'] / 1e6:.2f} gz), fcp {nv['browser']['fcp']:.0f} ms, lcp {nv['browser']['lcp']:.0f} ms")
    s = report["substack"]
    if "weight" in s:
        w, b = s["weight"], s["browser"]
        print(f"\n== substack ({s['url']}): html {w['html'] / 1024:.1f} KB ({w['html_gz'] / 1024:.1f} gz)  js {w['js'] / 1024:.0f} KB "
              f"({w['js_gz'] / 1024:.0f} gz, {w['js_files']} files)  ttfb {b['ttfb']:.0f}  fcp {b['fcp']:.0f}  lcp {b['lcp']:.0f}  load {b['load']:.0f}  script {b['script_ms']:.0f} ms")
    else:
        print("\n== substack:", s)


main()
