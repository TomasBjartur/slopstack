#!/usr/bin/env python3
"""Visual mode as a writer uses it, in real Chrome: every toolbar button on
and off again, the toolbar showing what applies at the caret, the link
dialog, Markdown shortcuts, Enter and Backspace in lists, quotes, headings
and code blocks, pasting formatted text (safely), undo, and reopening in
Visual mode. usage: tests/visual_test.py"""
import os, sys, time, sqlite3, subprocess, tempfile, base64, json
sys.path.insert(0, "/home/claude/web2/tests")
import editor_test as E
ED = E.ED
tmp = tempfile.mkdtemp(); dbpath = os.path.join(tmp, "blog.db"); E.BUDGET_DB = dbpath
srv = subprocess.Popen([os.path.join(E.ROOT, "build/server")], env=dict(os.environ, PORT=str(E.PORT), BLOG_DB=dbpath, BLOG_ORIGIN=E.BASE, BLOG_RP_ID="localhost"), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
E.wait_port(E.PORT)
db = sqlite3.connect(dbpath)
_, A = E.user(db, "ux@example.com")
E.http("POST", "/blogs", A, {"slug": "ux", "title": "UX"})
_, loc, _ = E.http("POST", "/dash/ux/posts", A, {"slug": "p", "title": "P"})
PID = loc.rsplit("/", 1)[1]
br = E.Browser(9505, f"{tmp}/c", A)
br.ws.call("Emulation.setDeviceMetricsOverride", {"width": 1100, "height": 900, "deviceScaleFactor": 1, "mobile": False})
br.js("localStorage.setItem('slop:vim', '0'); localStorage.setItem('slop:editor-mode', 'visual')")
def load(md):
    try:
        br.wait_saved(10)
    except Exception:
        pass
    E.http("POST", f"/edit/{PID}", A, {"title": "P", "body": md})
    br.open(f"{E.BASE}/edit/{PID}")
    if br.js("document.querySelector('.wys').hidden"):
        br.js("document.querySelector('.edit-tools .seg:nth-child(2)').click()")
    time.sleep(0.5)
def md():
    return br.js(f"String({ED}.view.text)")
def select(text, start=0, end=None):
    """Selects `text` (its [start, end) part) where it first occurs in the Visual view."""
    return br.js(f"""(() => {{ const root = document.querySelector('.wys'); const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT);
      let n; while ((n = w.nextNode())) {{ const i = n.data.indexOf({json.dumps(text)}); if (i >= 0) {{
        const r = document.createRange(); r.setStart(n, i + {start}); r.setEnd(n, i + {end if end is not None else 'text.length'.replace('text', json.dumps(text))});
        root.focus(); const s = getSelection(); s.removeAllRanges(); s.addRange(r); return true; }} }} return false; }})()""")
def button(cmd):
    br.js(f"document.querySelector('.fmt-bar button[data-cmd=\"{cmd}\"], .edit-tools button[data-cmd=\"{cmd}\"]').click()")
    time.sleep(0.25)
def shot(name):
    open(f"/tmp/claude-1001/ux/{name}.png", "wb").write(base64.b64decode(br.ws.call("Page.captureScreenshot", {"format": "png"})["data"]))
def pressed():
    return br.js("[...document.querySelectorAll('.edit-tools button[data-cmd]')].filter(b => b.getAttribute('aria-pressed') === 'true').map(b => b.dataset.cmd)")
def k(name, shift=False, ctrl=False):
    codes = {"Enter": 13, "Backspace": 8, "Tab": 9}
    mods = (8 if shift else 0) | (2 if ctrl else 0)
    if name in codes:
        base = {"key": name, "code": name, "windowsVirtualKeyCode": codes[name], "modifiers": mods}
        br.ws.call("Input.dispatchKeyEvent", dict(base, type="keyDown" if name == "Enter" else "rawKeyDown", **({"text": "\r"} if name == "Enter" else {})))
        br.ws.call("Input.dispatchKeyEvent", dict(base, type="keyUp"))
    else:
        br.ws.call("Input.dispatchKeyEvent", {"type": "keyDown", "key": name, "text": name, "modifiers": mods, "windowsVirtualKeyCode": ord(name.upper())})
        br.ws.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": name, "modifiers": mods, "windowsVirtualKeyCode": ord(name.upper())})
    time.sleep(0.08)
def typ(s):
    for ch in s:
        if ch == "\n": k("Enter")
        else: br.ws.call("Input.insertText", {"text": ch}); time.sleep(0.03)
    time.sleep(0.3)
def caret_end(text):
    select(text, len(text), len(text))
def caret_start(text):
    select(text, 0, 0)

fails = 0
def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))

def run():
    # With no choice made, the editor opens in Visual mode, and an empty
    # post says where to start.
    br.ws.call("Page.removeScriptToEvaluateOnNewDocument", {"identifier": br.ws.pin})
    br.open(f"{E.BASE}/edit/{PID}")
    br.js("localStorage.removeItem('slop:editor-mode')")
    br.open(f"{E.BASE}/edit/{PID}")
    time.sleep(0.5)
    check("with no choice made: Visual mode", br.js("!document.querySelector('.wys').hidden") is True)
    check("…an empty post shows 'Tell your story…'", br.js("getComputedStyle(document.querySelector('.wys p'), '::before').content") == '"Tell your story…"',
          br.js("getComputedStyle(document.querySelector('.wys p'), '::before').content"))
    # Visual mode chosen last time: after a reload it opens with the text
    # rendered and editable (it once opened before the renderer loaded).
    load("Hello world here.")
    br.open(f"{E.BASE}/edit/{PID}")
    time.sleep(0.5)
    check("Visual mode, reopened: blocks rendered and editable", br.js("!document.querySelector('.wys .wraw') && !!document.querySelector('.wys p')"),
          br.js("document.querySelector('.wys').innerHTML.slice(0, 200)"))
    # Every button on, then off again where it applies.
    for name, src, sel, cmd, want in [
        ("bold on", "Hello world here.", "world", "bold", "Hello **world** here."),
        ("bold off", "Hello **world** here.", "world", "bold", "Hello world here."),
        ("italic on", "Hello world here.", "world", "italic", "Hello *world* here."),
        ("italic off", "Hello *world* here.", "world", "italic", "Hello world here."),
        ("code on", "Hello world here.", "world", "code", "Hello `world` here."),
        ("code off", "Hello `world` here.", "world", "code", "Hello world here."),
        ("H2 on", "Hello world here.", "world", "h2", "## Hello world here."),
        ("H2 off", "## Hello world here.", "world", "h2", "Hello world here."),
        ("H3 on", "Hello world here.", "world", "h3", "### Hello world here."),
        ("H3 off", "### Hello world here.", "world", "h3", "Hello world here."),
        ("H2 to H3", "## Hello world here.", "world", "h3", "### Hello world here."),
        ("quote on", "Hello world here.", "world", "quote", "> Hello world here."),
        ("quote off", "> Hello world here.", "world", "quote", "Hello world here."),
        ("bulleted list on", "Hello world here.", "world", "ul", "- Hello world here."),
        ("bulleted list off", "- Hello world here.", "world", "ul", "Hello world here."),
        ("numbered list on", "Hello world here.", "world", "ol", "1. Hello world here."),
        ("numbered list off", "1. Hello world here.", "world", "ol", "Hello world here."),
        ("bulleted to numbered", "- Hello world here.", "world", "ol", "1. Hello world here."),
        ("code block on", "Start.\n\nsome code", "code", "pre", "Start.\n\n```\nsome code\n```"),
        ("code block off", "Start.\n\n```\nsome code\n```", "code", "pre", "Start.\n\nsome code"),
    ]:
        load(src)
        select(sel)
        button(cmd)
        check(f"toolbar: {name}", md() == want, (md(), want))
    # The toolbar shows what applies at the caret.
    for src, sel, want in [("Hello **world** here.", "world", ["bold"]), ("## Hello world", "world", ["h2"]), ("> quoted text", "quoted", ["quote"]),
                           ("- item one", "item", ["ul"]), ("Say `code` now", "code", ["code"]), ("A [link](https://x.y) here", "link", ["link"])]:
        load(src)
        select(sel, 1, 2)
        time.sleep(0.2)
        check(f"toolbar shows {want} at {src!r}", pressed() == want, pressed())
    # Links: the page's dialog, to add, change and remove.
    def dlg(url, action="ok"):
        br.js("(() => { const d = document.querySelector('dialog.modal[open]'); d.querySelector('input').value = " + json.dumps(url) + ";"
              " d.querySelector(" + json.dumps({"ok": "button.primary", "remove": "button:not([value])", "cancel": "button[value=cancel]"}[action]) + ").click(); })()")
        time.sleep(0.3)
    load("Hello world here.")
    select("world"); button("link")
    check("link: a dialog of the page's (not the browser's prompt)", br.js("!!document.querySelector('dialog.modal[open] input[type=url]')"))
    dlg("https://example.com")
    check("link: added", md() == "Hello [world](https://example.com) here.", md())
    select("world"); button("link")
    check("link: an existing link's address is filled in", br.js("document.querySelector('dialog.modal[open] input').value") == "https://example.com")
    dlg("https://other.org")
    check("link: changed", md() == "Hello [world](https://other.org) here.", md())
    select("world"); button("link"); dlg("", "remove")
    check("link: removed", md() == "Hello world here.", md())
    select("world"); button("link"); dlg("javascript:alert(1)")
    check("link: a javascript: address is refused, the dialog stays", br.js("!!document.querySelector('dialog.modal[open]')") and md() == "Hello world here.")
    dlg("", "cancel")
    # Typing.
    load("Start.")
    caret_end("Start."); typ("\n## Title\nAfter heading")
    check("typing: '## ' makes a heading; Enter after it, a paragraph", md() == "Start.\n\n## Title\n\nAfter heading", md())
    load("Start.")
    caret_end("Start."); typ("\n- one\ntwo\n\nout")
    check("typing: '- ' makes a list; Enter twice leaves it", md() == "Start.\n\n- one\n- two\n\nout", md())
    load("Start.")
    caret_end("Start."); typ("\n> quoted\nstill\n\nout")
    check("typing: '> ' makes a quote; Enter continues it; Enter on an empty line leaves it", md() == "Start.\n\n> quoted\n> \n> still\n\nout", md())
    load("- one\n- two")
    caret_start("two"); k("Backspace"); time.sleep(0.3)
    check("Backspace at a list item's start takes its bullet away", md() == "- one\n\ntwo", md())
    load("> quote")
    caret_start("quote"); k("Backspace"); time.sleep(0.3)
    check("Backspace at a quote's start unquotes it", md() == "quote", md())
    load("## Head")
    caret_start("Head"); k("Backspace"); time.sleep(0.3)
    check("Backspace at a heading's start makes it a paragraph", md() == "Head", md())
    load("Hello world.")
    caret_end("Hello"); k("b", ctrl=True); typ(" big"); k("b", ctrl=True); typ(" small")
    check("Ctrl+B on, typing, Ctrl+B off", md() == "Hello **big** small world.", md())
    load("Start.\n\n```\nx = 1\n```")
    caret_end("x = 1"); typ("\ny = 2")
    check("Enter in a code block stays in it", md() == "Start.\n\n```\nx = 1\ny = 2\n```", md())
    # Pasting keeps formatting, safely.
    def paste(html, text):
        br.js("(() => { const dt = new DataTransfer(); dt.setData('text/html', " + json.dumps(html) + "); dt.setData('text/plain', " + json.dumps(text) + ");"
              " document.querySelector('.wys').dispatchEvent(new ClipboardEvent('paste', {clipboardData: dt, bubbles: true, cancelable: true})); })()")
        time.sleep(0.4)
    load("Hello world.")
    caret_end("world.")
    paste('<p> Pasted <b>bold</b> and <a href="https://x.y">link</a></p><script>alert(1)</script><style>p{}</style><img src=x onerror=alert(2)>', " Pasted bold and link")
    # (A pasted paragraph's leading space goes, as CommonMark drops it.)
    check("paste: bold and links kept; scripts, styles and foreign images dropped", md() == "Hello world.Pasted **bold** and [link](https://x.y)", md())
    check("…nothing pasted runs", br.js("typeof window.__pwned") == "undefined" and not br.js("!!document.querySelector('.wys img[onerror], .wys script')"))
    load("Hello world.")
    caret_end("world.")
    paste('<b style="font-weight:normal;" id="docs-internal-guid-1"><p dir="ltr"><span style="font-weight:400"> Docs </span><span style="font-weight:700">bold</span><span style="font-weight:400"> and </span><span style="font-style:italic;font-weight:400">it</span></p></b>', " Docs bold and it")
    check("paste from Google Docs: its bold and italics (not all bold)", md() == "Hello world.Docs **bold** and *it*", md())
    # Undo takes a run of typing back at once.
    load("One.")
    caret_end("One."); typ(" Two")
    k("z", ctrl=True); time.sleep(0.4)
    a = md()
    k("y", ctrl=True); time.sleep(0.4)
    check("undo: the typed words at once; redo: back", a == "One." and md() == "One. Two", (a, md()))
    check("every change reached the document (the CRDT)", br.js(f"{ED}.doc().text() === String({ED}.view.text)"))
    errs = [e for e in br.ws.events if e.get("method") == "Runtime.exceptionThrown"]
    check("no errors", not errs, errs[:1])

try:
    run()
finally:
    br.proc.terminate(); srv.terminate(); srv.wait()
print(f"\n{fails} failure(s)")
sys.exit(1 if fails else 0)
