#!/usr/bin/env python3
"""Tab, nested lists and Preview, in real Chrome (and Preview's access over
HTTP).

- Markdown mode: Tab inserts a tab; with lines selected, indents each;
  Shift+Tab outdents; Esc then Tab leaves the editor; Vim's normal mode
  leaves Tab alone.
- Visual mode: Tab in a list item nests it, Shift+Tab brings it back; the
  Markdown says so (indented to the parent's text); nested lists written
  in Markdown survive an edit in Visual mode.
- Preview: the post as it will publish, for its authors only; for a
  published post, with the changes not yet published; the title as typed;
  the button sends unsent edits first.
usage: tests/tab_preview_test.py"""
import os, re, sqlite3, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import editor_test as E
from cdp import page_ws

ED = E.ED
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def key(br, k, shift=False):
    code = {"Tab": 9, "Escape": 27}[k]
    mods = 8 if shift else 0
    br.ws.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": k, "code": k, "windowsVirtualKeyCode": code, "modifiers": mods})
    br.ws.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": k, "code": k, "windowsVirtualKeyCode": code, "modifiers": mods})
    time.sleep(0.1)


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    E.BUDGET_DB = dbpath
    env = dict(os.environ, PORT=str(E.PORT), BLOG_DB=dbpath, BLOG_ORIGIN=E.BASE, BLOG_RP_ID="localhost")
    srv = subprocess.Popen([os.path.join(E.ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    br = None
    try:
        E.wait_port(E.PORT)
        db = sqlite3.connect(dbpath)
        _, a = E.user(db, "tab@example.com")
        _, c = E.user(db, "out@example.com")
        E.http("POST", "/blogs", a, {"slug": "tabs", "title": "Tabs"})
        _, loc, _ = E.http("POST", "/dash/tabs/posts", a, {"slug": "t", "title": "T"})
        pid = loc.rsplit("/", 1)[1]
        E.http("POST", f"/edit/{pid}", a, {"title": "T", "body": "one\ntwo\nthree"})
        br = E.Browser(9491, f"{tmp}/c", a)
        br.js("localStorage.setItem('slop:vim', '0'); localStorage.setItem('slop:editor-mode', 'markdown')")
        br.open(f"{E.BASE}/edit/{pid}")
        text = lambda: br.js(f"String({ED}.view.text)")

        # MARKDOWN MODE
        br.js("document.getElementById('editor').focus()")
        br.js(f"{ED}.view.select(3, 3)")
        key(br, "Tab")
        check("Tab inserts a tab", text() == "one\t\ntwo\nthree" and br.js("document.activeElement.id") == "editor", repr(text()))
        br.js(f"{ED}.view.select(0, 12)")  # "one\t\ntwo\nth"
        key(br, "Tab")
        check("Tab with lines selected indents each line", text() == "\tone\t\n\ttwo\n\tthree", repr(text()))
        s = br.js(f"(() => {{ const s = {ED}.view.sel(); return [s.a, s.b]; }})()")
        check("…the selection stays on the same text", s == [1, 15], s)
        key(br, "Tab", shift=True)
        check("Shift+Tab outdents them", text() == "one\t\ntwo\nthree", repr(text()))
        br.js(f"{ED}.view.select(4, 4)")
        key(br, "Tab", shift=True)
        check("Shift+Tab on a line without indentation changes nothing", text() == "one\t\ntwo\nthree", repr(text()))
        n = len(text())
        br.js(f"{ED}.view.select({n}, {n})")
        br.js("document.execCommand('insertText', false, '\\n      four')")
        n = len(text())
        br.js(f"{ED}.view.select({n}, {n})")
        key(br, "Tab", shift=True)
        check("…and removes up to four spaces", text() == "one\t\ntwo\nthree\n  four", repr(text()))
        br.js(f"{ED}.view.select(2, 2)")
        key(br, "Escape")
        key(br, "Tab")
        check("Esc then Tab leaves the editor (no tab typed)", br.js("document.activeElement.id") != "editor" and "\t\t" not in text(), (br.js("document.activeElement.id"), repr(text())))
        before = text()
        br.js("localStorage.setItem('slop:vim-ok', '1')")
        br.js("document.querySelector('.vim-toggle').click()")
        br.js("document.getElementById('editor').focus()")
        br.js(f"{ED}.view.select(1, 2)")
        key(br, "Tab")
        check("Vim normal mode: Tab changes nothing", text() == before, repr(text()))
        br.js("document.querySelector('.vim-toggle').click()")
        check("Tab edits reach the document (the CRDT)", br.js(f"{ED}.doc().text() === String({ED}.view.text)"))

        # VISUAL MODE: nesting lists.
        E.http("POST", f"/edit/{pid}", a, {"title": "T", "body": "- a\n- b\n- c\n\nAfter."})
        br.open(f"{E.BASE}/edit/{pid}")
        br.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
        time.sleep(0.5)
        br.js("""(() => { const li = document.querySelectorAll('.wys li')[1]; const t = li.firstChild;
            getSelection().collapse(t, 1); document.querySelector('.wys').focus(); })()""")
        key(br, "Tab")
        time.sleep(0.4)
        check("Visual: Tab in a list item nests it", text() == "- a\n  - b\n- c\n\nAfter.", repr(text()))
        check("…the page shows it nested", br.js("!!document.querySelector('.wys ul ul, .wys ul li ul')"))
        key(br, "Tab", shift=True)
        time.sleep(0.4)
        check("Visual: Shift+Tab brings it back", text() == "- a\n- b\n- c\n\nAfter.", repr(text()))
        # Nested lists written as Markdown survive an edit in Visual mode.
        br.js("document.querySelector('.edit-tools .seg:nth-child(1)').click()")
        E.http("POST", f"/edit/{pid}", a, {"title": "T", "body": "1. one\n   - inner\n   - more\n2. two\n\nEnd."})
        br.open(f"{E.BASE}/edit/{pid}")
        br.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
        time.sleep(0.5)
        br.js("""(() => { const p = [...document.querySelectorAll('.wys p')].pop(); getSelection().collapse(p.firstChild, 3);
            document.querySelector('.wys').focus(); document.execCommand('insertText', false, '!'); })()""")
        time.sleep(0.4)
        check("Visual: nested lists written in Markdown survive an edit elsewhere", text() == "1. one\n   - inner\n   - more\n2. two\n\nEnd!.", repr(text()))
        br.js("document.querySelector('.edit-tools .seg:nth-child(1)').click()")

        # PREVIEW over HTTP.
        st, _, body = E.http("GET", f"/edit/{pid}/preview", a)
        check("preview: for an author, the text rendered", st == 200 and "<li>two</li>" in body and "End!." in body and "Preview: as it will look" in body, (st, body[-300:]))
        st, _, _ = E.http("GET", f"/edit/{pid}/preview", c)
        check("preview: an outsider gets 404", st == 404, st)
        st, loc, _ = E.http("GET", f"/edit/{pid}/preview")
        check("preview: signed out, off to log in (and back)", st == 303 and loc == f"/login?next=%2Fedit%2F{pid}%2Fpreview", (st, loc))
        st, _, body = E.http("GET", f"/edit/{pid}/preview?title=%3Cb%3ENew%20title%3C%2Fb%3E", a)
        check("preview: the title as typed, escaped", "&lt;b&gt;New title&lt;/b&gt;" in body and "<b>New" not in body, body[:600])
        E.http("POST", f"/edit/{pid}", a, {"title": "T", "action": "publish"})
        E.http("POST", f"/edit/{pid}", a, {"title": "T", "body": "Changed after publishing."})
        _, _, pub = E.http("GET", "/b/tabs/t")
        _, _, prev = E.http("GET", f"/edit/{pid}/preview", a)
        check("preview of a published post: the changes not yet published (readers still see the old)", "Changed after publishing." in prev and "Changed after publishing." not in pub)

        # PREVIEW in Chrome: unsent edits go first; a tab of its own.
        br.open(f"{E.BASE}/edit/{pid}")
        br.js("document.getElementById('editor').focus()")
        br.js(f"{ED}.view.select(0, 0)")
        br.js("document.execCommand('insertText', false, 'Just typed. ')")
        br.js("document.querySelector('textarea.title').value = 'Typed title'")
        br.js("document.getElementById('preview').click()")
        time.sleep(3)
        import json, urllib.request
        tabs = [t for t in json.load(urllib.request.urlopen("http://127.0.0.1:9491/json/list")) if t["type"] == "page" and "/preview" in t["url"]]
        check("the Preview button opens a tab with the preview", len(tabs) == 1, [t["url"] for t in tabs])
        if tabs:
            from cdp import WS
            p2 = WS(tabs[0]["webSocketDebuggerUrl"])
            html = p2.call("Runtime.evaluate", {"expression": "document.querySelector('.body').innerText + '|' + document.querySelector('h1').innerText", "returnByValue": True})["result"].get("value", "")
            check("…showing what was just typed, and the title as typed", "Just typed." in html and "Typed title" in html, html[:200])
        errs = [e for e in br.ws.events if e.get("method") == "Runtime.exceptionThrown"]
        check("no errors in the editor", not errs, errs[:1])
    finally:
        if br:
            br.close()
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


main()
