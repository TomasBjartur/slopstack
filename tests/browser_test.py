#!/usr/bin/env python3
"""Real-browser passkey test: headless Chrome with a virtual authenticator.

Drives Chrome over the DevTools protocol (tests/cdp.py, stdlib only).
Exercises the real web/passkey.js, the CSP, the session cookie and Chrome's
own WebAuthn encodings end to end: sign-up through the mailed link, log
out, log in, recovery (a second passkey), then a first blog and post
written, published and updated through the real pages.

usage: tests/browser_test.py   (needs tools/setup_chrome.sh once)
"""
import os, re, sqlite3, subprocess, sys, tempfile, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from cdp import wait_port, start_chrome, page_ws

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PORT = 8083
CDP_PORT = 9431
BASE = f"http://localhost:{PORT}"
fails = 0


def check(name, ok, detail=""):
    global fails
    fails += 0 if ok else 1
    print(("PASS " if ok else "FAIL ") + name + ("" if ok else f"   {detail}"))


def main():
    tmp = tempfile.mkdtemp()
    dbpath = os.path.join(tmp, "blog.db")
    env = dict(os.environ, PORT=str(PORT), BLOG_DB=dbpath, BLOG_ORIGIN=BASE, BLOG_RP_ID="localhost")
    env.pop("BLOG_SIGNUP_DIRECT", None)
    srv = subprocess.Popen([os.path.join(ROOT, "build/server")], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    chrome = start_chrome(CDP_PORT, f"{tmp}/chrome")
    try:
        wait_port(PORT)
        run(dbpath)
    finally:
        chrome.terminate()
        srv.terminate()
        chrome.wait()
        srv.wait()
    print(f"\n{fails} failure(s)")
    sys.exit(1 if fails else 0)


def run(dbpath):
    ws = page_ws(CDP_PORT)
    ws.call("Page.enable")
    ws.call("Runtime.enable")
    ws.call("Log.enable")
    ws.call("Network.enable")
    ws.call("WebAuthn.enable")
    auth = ws.call("WebAuthn.addVirtualAuthenticator", {"options": {
        "protocol": "ctap2", "transport": "internal", "hasResidentKey": True, "hasUserVerification": True,
        "isUserVerified": True, "automaticPresenceSimulation": True}})["authenticatorId"]
    db = sqlite3.connect(dbpath, timeout=5)

    def js(expr):
        r = ws.call("Runtime.evaluate", {"expression": expr, "awaitPromise": True, "returnByValue": True})
        return r.get("result", {}).get("value")

    def loaded():
        for _ in range(100):
            if js("document.readyState") == "complete":
                return
            time.sleep(0.05)

    def go(url):
        ws.call("Page.navigate", {"url": url})
        time.sleep(0.3)
        loaded()

    def text():
        return js("document.body ? document.body.innerText : ''") or ""

    def wait_path(path, secs=10):
        for _ in range(secs * 10):
            if js("location.pathname") == path:
                loaded()
                return True
            time.sleep(0.1)
        return False

    def wait_text(s, secs=5):
        for _ in range(secs * 10):
            if s in text():
                return True
            time.sleep(0.1)
        return False

    def mail_link(email):
        row = db.execute("SELECT body FROM outbox WHERE to_email = ? ORDER BY id DESC LIMIT 1", (email,)).fetchone()
        m = row and re.search(r"(http://localhost:\d+/verify\?t=[0-9a-f]{64})", row[0])
        return m.group(1) if m else None

    # SIGN-UP through the real form (the address typed in capitals).
    go(BASE + "/signup")
    js("document.querySelector('input[name=email]').value = 'Eve@Example.com';"
       "document.querySelector('input[name=name]').value = 'Eve';"
       "document.querySelector('input[name=handle]').value = 'eve';"
       "document.querySelector('form[action=\"/signup\"]').submit()")
    check("sign-up form submitted", wait_text("Check your email"), text()[:300])
    link = mail_link("eve@example.com")
    check("the link is mailed (address lower-cased)", link is not None)

    # Create a passkey with the virtual authenticator.
    go(link)
    check("verify page has the button", js("!!document.getElementById('passkey-register')") is True, text()[:300])
    js("document.getElementById('passkey-register').click()")
    check("registered and redirected to /dash", wait_path("/dash"), js("location.href") + " " + text()[:300])
    check("the dashboard shows", "Your blogs" in text(), text()[:300])
    creds = ws.call("WebAuthn.getCredentials", {"authenticatorId": auth})["credentials"]
    check("authenticator holds one resident credential", len(creds) == 1 and creds[0]["isResidentCredential"], creds)
    check("the account has its handle and the lower-cased email",
          db.execute("SELECT email, name, handle FROM user").fetchall() == [("eve@example.com", "Eve", "eve")])
    cookies = ws.call("Network.getCookies", {"urls": [BASE]})["cookies"]
    sid = [c for c in cookies if c["name"] == "sid"]
    # (Secure is set only when BLOG_ORIGIN is https; this origin is http.)
    check("session cookie is HttpOnly, SameSite=Strict",
          len(sid) == 1 and sid[0]["httpOnly"] and sid[0]["sameSite"] == "Strict", sid)
    check("script cannot read the session cookie", "sid=" not in (js("document.cookie") or ""))
    go(link)
    check("the used link no longer shows the button", js("!!document.getElementById('passkey-register')") is False, text()[:200])

    # Log out, then log in with the passkey.
    go(BASE + "/dash")
    js("document.querySelector('form[action=\"/logout\"]').submit()")
    wait_path("/")
    go(BASE + "/dash")
    check("logged out: /dash sends to /login", js("location.pathname") == "/login", js("location.href"))
    check("the old session is gone", db.execute("SELECT count(*) FROM session").fetchone()[0] == 0)
    js("document.getElementById('passkey-login').click()")
    check("passkey login redirected to /dash", wait_path("/dash"), js("location.href") + " " + text()[:300])

    # RECOVERY: a lost device. The login page's form mails a link; it adds a
    # second passkey to the same account.
    ws.call("Network.clearBrowserCookies")
    go(BASE + "/login")
    js("document.querySelector('details').open = true;"
       "document.querySelector('form[action=\"/recover\"] input[name=email]').value = 'eve@example.com';"
       "document.querySelector('form[action=\"/recover\"]').submit()")
    check("recovery form: check your email", wait_text("Check your email"), text()[:300])
    rlink = mail_link("eve@example.com")
    check("a recovery link is mailed", rlink is not None and rlink != link, rlink)
    go(rlink)
    check("recovery page offers to add a passkey", "Add a new passkey" in text(), text()[:300])
    js("document.getElementById('passkey-register').click()")
    check("second passkey registered, signed in", wait_path("/dash"), js("location.href") + " " + text()[:300])
    creds = ws.call("WebAuthn.getCredentials", {"authenticatorId": auth})["credentials"]
    rows = db.execute("SELECT DISTINCT user_id FROM credential").fetchall()
    check("two passkeys, one account", len(creds) == 2 and len(rows) == 1 and db.execute("SELECT count(*) FROM credential").fetchone()[0] == 2,
          (len(creds), rows))

    # Use the app as a real user: name a blog, press Write, type, Publish.
    go(BASE + "/dash")
    check("new user is asked to name a blog", "Welcome" in text(), text()[:300])
    js("document.querySelector('form[action=\"/blogs\"] [name=title]').value = 'Eve <writes>';"
       "document.querySelector('form[action=\"/blogs\"] button').click()")
    check("blog created, address from its name", wait_path("/dash/eve-writes"), js("location.href"))
    js("[...document.querySelectorAll('header button')].find(b => b.innerText.trim() === 'Write').click()")
    for _ in range(50):
        if (js("location.pathname") or "").startswith("/edit/"):
            break
        time.sleep(0.1)
    check("Write opens the editor on a new draft", (js("location.pathname") or "").startswith("/edit/"), js("location.href"))
    for _ in range(100):
        if js("!!(document.getElementById('editor') || {}).ed && document.getElementById('editor').ed.loaded()"):
            break
        time.sleep(0.1)
    js("const ti = document.querySelector('[name=title]'); ti.value = 'Hello, world'; ti.dispatchEvent(new Event('input', {bubbles: true}));"
       "const t = document.getElementById('editor'); t.value = 'First line\\n\\n<img src=x onerror=alert(1)>';"
       "t.dispatchEvent(new Event('input', {bubbles: true}));"
       "document.querySelector('button[value=publish]').click()")
    check("Publish lands on the live post, address from its title", wait_path("/b/eve-writes/hello-world"), js("location.href"))
    body = text()
    check("with a notice that it is live", "Your post is live" in body, body[:300])
    check("published post renders", "First line" in body, body[:300])
    check("XSS payload is inert text", js("document.querySelectorAll('.body img').length") == 0 and
          "<img src=x onerror=alert(1)>" in body, body[:400])

    # Update (the post is published): saved in place, live at once.
    go(BASE + "/edit/%d" % db.execute("SELECT id FROM post WHERE slug = 'hello-world'").fetchone()[0])
    for _ in range(100):
        time.sleep(0.1)
        if (js("location.pathname") or "").startswith("/edit/") and js("!!(document.getElementById('editor') || {}).ed && document.getElementById('editor').ed.loaded()"):
            break
    check("the published post opens in the editor", (js("location.pathname") or "").startswith("/edit/") and "First line" in (js("document.getElementById('editor').value") or ""),
          js("location.href"))
    js("window.__marker = 9;"
       "const t2 = document.getElementById('editor'); t2.value = t2.value + '\\n\\nSecond thought.';"
       "t2.dispatchEvent(new Event('input', {bubbles: true}));")
    time.sleep(1)
    js("document.querySelector('button[value=publish]').click()")
    for _ in range(50):
        time.sleep(0.2)
        if js("document.getElementById('sync-status').textContent") == "Updated · live now":
            break
    check("Update saves in place", js("window.__marker") == 9 and js("document.getElementById('sync-status').textContent") == "Updated · live now",
          js("document.getElementById('sync-status').textContent"))
    go(BASE + "/b/eve-writes/hello-world")
    body = text()
    check("the update is live", "Second thought." in body, body[:400])
    check("no notice on a plain visit", "Your post is live" not in body)
    js("document.querySelector('.byline a').click()")
    wait_path("/u/eve")
    check("byline links to the author page", js("location.pathname") == "/u/eve" and "Hello, world" in text(), js("location.href"))

    # Nothing was blocked by the CSP, and there were no console errors.
    # (The used link's 404, visited on purpose above, is expected.)
    errors = [e for e in ws.events if e.get("method") in ("Log.entryAdded", "Runtime.exceptionThrown")
              and (e["params"].get("entry", {}).get("level") == "error" or e.get("method") == "Runtime.exceptionThrown")
              and e["params"].get("entry", {}).get("url") != link]
    check("no console errors or CSP violations", not errors, [e["params"] for e in errors][:3])


main()
