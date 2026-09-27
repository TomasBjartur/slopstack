#!/usr/bin/env python3
"""Logging in, as a person does it, in real Chrome with a virtual passkey
authenticator:

- "Log in" on any page opens a dialog over it (a passkey login is one
  tap); afterwards the same page, signed in. Cancel and Esc close it.
- From a post's "Log in to comment": back on the post, able to comment.
- A page that needs a login (the editor) sends you to log in, then back to
  it (?next=); logging in on /login with no destination: the dashboard.
- /login when signed in goes on; ?next= never leads off the site.
- Signing up from somewhere (?next=) comes back there after the passkey
  is made; sign-up errors say which field is wrong.
usage: tests/login_test.py"""
import os, re, sqlite3, subprocess, sys, tempfile, time, urllib.parse

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import start_chrome, page_ws, wait_port

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8087
BASE = f"http://localhost:{PORT}"
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_ORIGIN=BASE, BLOG_RP_ID="localhost", BLOG_SIGNUP_DIRECT="1")
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    chrome = start_chrome(9521, f"{tmp}/chrome")
    try:
        wait_port(PORT)
        run(sqlite3.connect(dbpath, timeout=5))
    finally:
        chrome.terminate()
        srv.terminate()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


def run(db):
    ws = page_ws(9521)
    for d in ("Page", "Runtime", "Log", "Network", "WebAuthn"):
        ws.call(d + ".enable")
    ws.call("WebAuthn.addVirtualAuthenticator", {"options": {
        "protocol": "ctap2", "transport": "internal", "hasResidentKey": True, "hasUserVerification": True,
        "isUserVerified": True, "automaticPresenceSimulation": True}})

    def js(e):
        return ws.call("Runtime.evaluate", {"expression": e, "awaitPromise": True, "returnByValue": True}).get("result", {}).get("value")

    def go(url):
        ws.call("Page.navigate", {"url": BASE + url})
        time.sleep(0.3)
        for _ in range(100):
            if js("document.readyState") == "complete":
                break
            time.sleep(0.05)
        time.sleep(0.2)

    def until(e, secs=8):
        for _ in range(secs * 10):
            v = js(e)
            if v:
                return v
            time.sleep(0.1)
        return None

    def here():
        return js("location.pathname + location.search")

    def signed_in():
        return bool(js("!!document.querySelector('form[action=\"/logout\"]')"))

    def logout():
        js("document.querySelector('form[action=\"/logout\"]').submit()")
        until("location.pathname === '/' && document.readyState === 'complete'")
        time.sleep(0.3)

    # An account (signed up through the form) and a published post.
    go("/signup")
    js("document.querySelector('input[name=name]').value = 'Ada'; document.querySelector('input[name=handle]').value = 'ada';"
       "document.querySelector('input[name=email]').value = 'ada@example.com'; document.querySelector('form[action=\"/signup\"]').submit()")
    until("location.pathname === '/verify'")
    time.sleep(0.3)
    js("document.getElementById('passkey-register').click()")
    check("sign-up: to the dashboard", until("location.pathname === '/dash'") is not None, here())
    time.sleep(0.3)
    js("document.querySelector('input[name=title]').value = 'Notes'; document.querySelector('form[action=\"/blogs\"]').submit()")
    until("location.pathname.startsWith('/dash/')")
    time.sleep(0.3)
    slug = js("location.pathname.split('/')[2]")
    js("document.querySelector('form[action$=\"/posts\"] button').click()")
    until("location.pathname.startsWith('/edit/')")
    pid = js("location.pathname.split('/')[2]")
    time.sleep(1.0)
    js("document.querySelector('textarea.title').value = 'Hello'; document.querySelector('button[value=publish]').click()")
    until("location.pathname.startsWith('/b/')")
    post = js("location.pathname")
    logout()
    check("logged out: on the home page", here() == "/" and not signed_in(), here())

    # THE DIALOG from the nav, on the home page.
    js("window.__stay = 1")
    import base64
    js("document.querySelector('header.top a[href=\"/login\"]').click()")
    time.sleep(0.5)
    open("/tmp/claude-1001/ux/login_dialog.png", "wb").write(base64.b64decode(ws.call("Page.captureScreenshot", {"format": "png"})["data"]))
    check("Log in opens a dialog over the page (no page load)", js("!!document.querySelector('dialog.modal[open] #login-go')") and js("window.__stay") == 1 and here() == "/", here())
    ws.call("Input.dispatchKeyEvent", {"type": "rawKeyDown", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})
    ws.call("Input.dispatchKeyEvent", {"type": "keyUp", "key": "Escape", "code": "Escape", "windowsVirtualKeyCode": 27})
    time.sleep(0.3)
    check("Esc closes it", not js("!!document.querySelector('dialog.modal[open]')"))
    js("document.querySelector('header.top a[href=\"/login\"]').click()")
    time.sleep(0.4)
    js("document.getElementById('login-cancel').click()")
    time.sleep(0.2)
    check("Cancel closes it", not js("!!document.querySelector('dialog.modal[open]')"))
    js("document.querySelector('header.top a[href=\"/login\"]').click()")
    time.sleep(0.4)
    js("document.getElementById('login-go').click()")
    until("!!document.querySelector('form[action=\"/logout\"]')")
    check("logging in there: the same page, signed in", here() == "/" and signed_in(), here())
    logout()

    # From a post: "Log in" to comment, then back on the post.
    go(post)
    js("document.querySelector('#comments a[href=\"/login\"]').click()")
    time.sleep(0.4)
    check("a post's 'Log in' (to comment) opens the dialog", js("!!document.querySelector('dialog.modal[open]')"))
    js("document.getElementById('login-go').click()")
    until("!!document.querySelector('#cform textarea')")
    check("…then the post again, with the comment box", here() == post and js("!!document.querySelector('#cform textarea')"), here())
    logout()

    # A page that needs a login: to /login?next=, then back to it.
    go(f"/edit/{pid}")
    check("the editor, signed out: to log in, remembering where", here() == "/login?next=" + urllib.parse.quote(f"/edit/{pid}", safe=""), here())
    js("document.getElementById('passkey-login').click()")
    check("…after logging in: the editor", until(f"location.pathname === '/edit/{pid}'") is not None, here())
    logout()
    go("/login")
    js("document.getElementById('passkey-login').click()")
    check("the log-in page with nowhere to go: the dashboard", until("location.pathname === '/dash'") is not None, here())
    go(f"/login?next={urllib.parse.quote(post)}")
    check("/login when signed in: on to ?next=", here() == post, here())
    go("/login")
    check("/login when signed in, no next: the dashboard", here() == "/dash", here())
    go("/login?next=//evil.example/x")
    check("?next=//another site is ignored (the dashboard)", here() == "/dash", here())
    go("/login?next=" + urllib.parse.quote("/\\evil.example/x", safe=""))
    check("?next=/\\another site is ignored by the server too", js("location.origin") == BASE and here() == "/dash", js("location.href"))
    logout()
    for bad in ["https://evil.example/", "//evil.example/", "/\\evil.example"]:
        go("/login?next=" + urllib.parse.quote(bad, safe=""))
        js("document.getElementById('passkey-login').click()")
        until("location.pathname !== '/login'")
        time.sleep(0.3)
        check(f"?next={bad} after logging in: stays on the site (the dashboard)", js("location.origin") == BASE and here() == "/dash", js("location.href"))
        logout()

    # The session ends while writing: the editor says so and offers to log
    # in; logging in there saves the text, and the page stays.
    go("/login")
    js("document.getElementById('passkey-login').click()")
    until("location.pathname === '/dash'")
    go(f"/edit/{pid}")
    until("document.getElementById('editor').ed && document.getElementById('editor').ed.loaded()")
    if not js("document.querySelector('.wys').hidden"):
        js("document.querySelector('.edit-tools .seg:nth-child(1)').click()")
    db.execute("DELETE FROM session")
    db.commit()
    js("window.__stay = 2; const t = document.getElementById('editor'); t.focus(); document.execCommand('insertText', false, 'Written while logged out. ')")
    offer = until("document.querySelector('#sync-status a[href=\"/login\"]') ? document.getElementById('sync-status').textContent : ''", 10)
    check("session ended while writing: the editor says so and offers to log in", offer and "logged out" in offer, offer)
    js("document.querySelector('#sync-status a[href=\"/login\"]').click()")
    time.sleep(0.4)
    js("document.getElementById('login-go').click()")
    saved = until("document.getElementById('sync-status').textContent.startsWith('Saved')", 10)
    check("…logging in there saves the text, and the page stays", saved and js("window.__stay") == 2 and not js("!!document.querySelector('dialog.modal[open]')"),
          (js("document.getElementById('sync-status').textContent"), js("window.__stay")))
    time.sleep(0.5)
    check("…the text reached the server", "Written while logged out." in (js(f"fetch('/edit/{pid}/preview').then(r => r.text())") or ""))
    go("/")
    logout()

    # Signing up from a post comes back to it.
    go(f"/signup?next={urllib.parse.quote(post)}")
    js("document.querySelector('input[name=name]').value = 'Bo'; document.querySelector('input[name=handle]').value = 'bob';"
       "document.querySelector('input[name=email]').value = 'bob@example.com'; document.querySelector('form[action=\"/signup\"]').submit()")
    until("location.pathname === '/verify'")
    time.sleep(0.3)
    check("…the passkey page keeps where to go", "next=" in js("location.search"), js("location.search"))
    js("document.getElementById('passkey-register').click()")
    check("sign-up from a post: back on the post once the passkey is made", until(f"location.pathname === {post!r}") is not None, here())
    logout()

    # Sign-up errors name the field.
    go("/signup")
    js("document.querySelector('input[name=name]').value = 'Cy'; document.querySelector('input[name=handle]').value = 'cy';"
       "document.querySelector('input[name=email]').value = 'cy@example.com'; document.querySelector('form[action=\"/signup\"]').submit()")
    time.sleep(1)
    note = js("(document.querySelector('.notice') || {}).textContent") or ""
    check("sign-up error names the field (username), not the others", "Username" in note and "Email" not in note and "Your name" not in note, note)
    check("…and keeps what was typed", js("document.querySelector('input[name=name]').value") == "Cy")

    errs = [e for e in ws.events if e.get("method") == "Runtime.exceptionThrown"
            or (e.get("method") == "Log.entryAdded" and e["params"]["entry"]["level"] == "error" and not re.search(r"status of (400|403|404)", e["params"]["entry"].get("text", "")))]
    check("no errors or CSP violations", not errs, [str(e)[:200] for e in errs[:2]])


main()
