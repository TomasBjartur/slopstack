// The application: routes, sessions, sign-up and passkeys, blogs, posts.
// How each part runs (DESIGN.md): Datastar SSR for everything bound by the
// database (reading pages, dashboards, account flows: small patches of
// server-rendered HTML); the editor is the browser's own (web/editor.js).
//
// Where the guarantees come from:
// - every write goes through authz::authorize (proved policy) and
//   Store::write (the facts re-checked in its transaction, the budget);
// - every POST passes authz::csrf (proved) before anything else;
// - a session is issued only by Store::session_for, which takes a
//   webauthn::Passed (made only when a proved decision said yes);
// - all HTML comes from pages.rs (escaped text) and markdown::render
//   (proved allowed markup).
// The rest (routing, forms, flows) is tested: tests/*.py.
use crate::authz::{authorize, csrf, Permit};
use crate::db::{No, Q, Store};
use crate::form::{email_ok, slugify, Form};
use crate::html::Markup;
use crate::pages::{self, H};
use crate::resp::{self, hex, unhex, Cache};
use crate::server::{App, Ctx, Request};
use crate::spec_authz::{Action, Facts};
use crate::sys::crypto::{p256_verify, sha256};
use crate::sys::sqlite::{DbErr, Val};
use crate::webauthn as wa;
use std::collections::HashMap;
use std::rc::Rc;

/// An emailed link works once, for 30 minutes; a passkey challenge for 5.
const TOKEN_MS: u64 = 30 * 60 * 1000;
const CHALLENGE_MS: u64 = 5 * 60 * 1000;
/// Mail to one address: at most 3 an hour (sign-up and recovery together).
const MAILS_PER_HOUR: i64 = 3;
/// Login challenges are made for anyone who asks (before sign-in), and each
/// is a row until it expires: at most this many a minute (per process).
const CHALLENGES_PER_MINUTE: u32 = 10_000;
/// Rendered posts kept in memory (the Markdown is rendered on read).
const CACHE_BYTES: usize = 256 << 20;
/// Feed pages: 20 items, pages 1 to 500.
const PAGE_ITEMS: i64 = 20;
const PAGES_MAX: u64 = 500;
/// A post's text (the form's field): 8 MiB (a long novel is ~6 MB).
const TEXT_MAX: usize = 8 << 20;
/// Texts longer than this are not put in the edit page (the editor loads
/// them): 1 MiB.
const EMBED_MAX: usize = 1 << 20;

pub struct Conf {
    /// What browsers report, e.g. "https://blog.example".
    pub origin: String,
    pub rp_id: String,
    pub rp_hash: [u8; 32],
    /// Test mode without mail (BLOG_SIGNUP_DIRECT=1): sign-up goes straight
    /// to making a passkey; the address is not checked.
    pub direct: bool,
    pub secure: bool,
}

impl Conf {
    pub fn new(origin: &str, rp_id: &str, direct: bool) -> Conf {
        Conf { origin: origin.to_string(), rp_id: rp_id.to_string(), rp_hash: sha256(rp_id.as_bytes()), direct, secure: origin.starts_with("https://") }
    }

    pub fn from_env(port: u16) -> Conf {
        let origin = std::env::var("BLOG_ORIGIN").unwrap_or_else(|_| format!("http://localhost:{port}"));
        let rp_id = std::env::var("BLOG_RP_ID").unwrap_or_else(|_| "localhost".into());
        Conf::new(&origin, &rp_id, std::env::var("BLOG_SIGNUP_DIRECT").as_deref() == Ok("1"))
    }
}

/// Rendered posts by id, as of their updated_ms. Bounded by bytes: past
/// the budget, entries are dropped until it fits.
struct PostCache {
    map: HashMap<u64, (i64, Rc<Markup>)>,
    bytes: usize,
}

impl PostCache {
    fn get(&self, id: u64, updated_ms: i64) -> Option<Rc<Markup>> {
        match self.map.get(&id) {
            Some((u, m)) if *u == updated_ms => Some(m.clone()),
            _ => None,
        }
    }

    fn put(&mut self, id: u64, updated_ms: i64, m: Rc<Markup>) {
        let n = m.len();
        if n > CACHE_BYTES / 4 {
            return;
        }
        if let Some((_, old)) = self.map.remove(&id) {
            self.bytes -= old.len();
        }
        while self.bytes + n > CACHE_BYTES {
            let k = *self.map.keys().next().expect("bytes > 0 means entries");
            let (_, old) = self.map.remove(&k).expect("key just seen");
            self.bytes -= old.len();
        }
        self.bytes += n;
        self.map.insert(id, (updated_ms, m));
    }
}

pub struct Site {
    pub st: Store,
    docs: crate::docs::Docs,
    conf: Conf,
    cache: PostCache,
    /// Login challenges made this minute (CHALLENGES_PER_MINUTE).
    minute: u64,
    challenges: u32,
    swept_ms: u64,
}

/// The signed-in user.
struct Who {
    id: u64,
    token: [u8; 32],
}

/// One request, as the handlers see it.
struct R<'a, 'b> {
    req: &'a Request<'a>,
    cx: &'a mut Ctx<'b>,
    path: &'a str,
    query: &'a str,
    who: Option<Who>,
    head_only: bool,
    /// A Datastar request (answer with patches, not a page).
    ds: bool,
}

impl<'a, 'b> R<'a, 'b> {
    fn uid(&self) -> u64 {
        self.who.as_ref().map_or(0, |w| w.id)
    }
    fn signed_in(&self) -> bool {
        self.who.is_some()
    }
    fn now(&self) -> u64 {
        self.cx.now_ms
    }
    fn random<const N: usize>(&mut self) -> [u8; N] {
        let mut b = [0u8; N];
        (self.cx.random)(&mut b);
        b
    }
    fn page_no(&self) -> u64 {
        let q = Form::parse(self.query.as_bytes());
        q.and_then(|f| f.num("page")).unwrap_or(1).clamp(1, PAGES_MAX)
    }
}

fn no_code(e: No) -> u16 {
    match e {
        No::Denied => 403,
        No::Conflict => 409,
        No::Budget => 429,
        No::Bad => 400,
        No::Error => 503,
    }
}

fn db_code(e: DbErr) -> u16 {
    match e {
        DbErr::Conflict => 409,
        _ => 503,
    }
}

type Res = Result<(), u16>;

impl Site {
    pub fn new(st: Store, conf: Conf) -> Site {
        Site { st, docs: crate::docs::Docs::new(), conf, cache: PostCache { map: HashMap::new(), bytes: 0 }, minute: 0, challenges: 0, swept_ms: 0 }
    }

    // RESPONSES
    fn html(&self, r: &mut R, out: &mut Vec<u8>, code: u16, f: impl FnOnce(&mut H)) {
        let nonce = wa::b64url_encode(&r.random::<16>());
        let mut h = H::new(nonce);
        f(&mut h);
        let cache = if r.signed_in() || code != 200 { Cache::NoStore } else { Cache::Revalidate };
        resp::head_wasm(out, code, "text/html; charset=utf-8", cache, Some(&h.nonce), h.wasm);
        resp::body(out, &h.b, !r.head_only);
    }

    fn error_page(&self, r: &mut R, out: &mut Vec<u8>, code: u16) {
        out.clear();
        if r.ds {
            // A Datastar request that failed: say so where the page shows it.
            let msg: &[u8] = match code {
                403 => b"<div id=\"flash\" class=\"notice bad\" role=\"alert\">Not allowed. Reload the page and try again.</div>",
                429 => b"<div id=\"flash\" class=\"notice bad\" role=\"alert\">That was a lot of changes in a minute. Wait a moment.</div>",
                _ => b"<div id=\"flash\" class=\"notice bad\" role=\"alert\">That did not work. Try again.</div>",
            };
            resp::patches(out, &[("body", "prepend", msg)]);
            return;
        }
        let signed = r.signed_in();
        self.html(r, out, code, |h| pages::error(h, code, signed));
    }

    fn text(out: &mut Vec<u8>, code: u16, body: &[u8], extra: &[u8]) {
        resp::whole(out, code, "text/plain; charset=utf-8", Cache::NoStore, None, extra, body, true);
    }

    fn json(out: &mut Vec<u8>, body: &[u8]) {
        resp::whole(out, 200, "application/json", Cache::NoStore, None, b"", body, true);
    }

    // SESSIONS
    fn who(&mut self, req: &Request, now: u64) -> Option<Who> {
        let c = req.header(b"cookie")?;
        let i = c.windows(4).position(|w| w == b"sid=")?;
        if i > 0 && c[i - 1] != b' ' && c[i - 1] != b';' {
            return None;
        }
        let v = c.get(i + 4..i + 68)?;
        let t: [u8; 32] = unhex(v)?.try_into().ok()?;
        let (id, _) = self.st.session(&t, now)?;
        Some(Who { id, token: t })
    }


    fn permit(&mut self, r: &R, blog: u64, post: u64, a: Action) -> Result<Permit, u16> {
        let f: Facts = self.st.facts(r.uid(), blog, post).map_err(db_code)?;
        // Refused: a page is "not found" (whether it exists is not said);
        // a change is "forbidden".
        let read = r.req.method != b"POST";
        authorize(f, a).ok_or(if read && r.signed_in() { 404u16 } else { 403 })
    }

    fn blog_id(&mut self, slug: &str) -> Result<(u64, String), u16> {
        let mut b = None;
        self.st.q(Q::BlogBySlug, &[Val::Text(slug.as_bytes())], |row| b = Some((row.int(0) as u64, row.text(1).to_string()))).map_err(db_code)?;
        b.ok_or(404u16)
    }

    // ROUTES
    fn route(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let parts: Vec<&str> = r.path.split('/').skip(1).collect();
        let get = r.req.method == b"GET" || r.req.method == b"HEAD";
        if !get {
            if r.req.method != b"POST" {
                return Err(405);
            }
            // THE CSRF LAW (proved): no state changes without same-origin.
            if !csrf(r.req.header(b"sec-fetch-site")) {
                return Err(403);
            }
            // The editor's sync: binary, not a form.
            if let ["edit", id, "sync"] = parts.as_slice() {
                return self.sync(r, id, out);
            }
            let form = Form::parse(r.req.body).ok_or(400u16)?;
            return self.post(r, &parts, &form, out);
        }
        match parts.as_slice() {
            [""] => self.home(r, out),
            ["s", name] => self.asset(r, name, out),
            ["b", blog] => self.blog(r, blog, out),
            ["b", blog, post] => self.post_page(r, blog, post, out),
            ["u", handle] => self.author(r, handle, out),
            ["signup"] => {
                if r.signed_in() {
                    resp::redirect(out, "/dash", b"");
                    return Ok(());
                }
                self.html(r, out, 200, |h| pages::signup(h, "", "", "", ""));
                Ok(())
            }
            ["login"] | ["recover"] => {
                let s = r.signed_in();
                self.html(r, out, 200, |h| pages::login(h, s));
                Ok(())
            }
            ["handle"] => self.handle_check(r, out),
            ["verify"] => self.verify(r, out),
            ["dash"] => self.dash(r, out),
            ["dash", blog] => self.dash_blog(r, blog, "", out),
            ["edit", id] => self.edit(r, id, out),
            _ => Err(404),
        }
    }

    fn post(&mut self, r: &mut R, parts: &[&str], f: &Form, out: &mut Vec<u8>) -> Res {
        match parts {
            ["signup"] => self.signup(r, f, out),
            ["recover"] => self.recover(r, f, out),
            ["passkey", "register", "options"] => self.register_options(r, f, out),
            ["passkey", "register"] => self.register(r, f, out),
            ["passkey", "login", "options"] => self.login_options(r, out),
            ["passkey", "login"] => self.login(r, f, out),
            ["logout"] => {
                if let Some(w) = &r.who {
                    self.st.end_session(&w.token);
                }
                resp::redirect(out, "/", &resp::cookie(None, self.conf.secure));
                Ok(())
            }
            ["write"] if !r.signed_in() => {
                resp::redirect(out, "/dash", b"");
                Ok(())
            }
            // Everything else changes content: a session first.
            _ if !r.signed_in() => Err(403),
            ["blogs"] => self.new_blog(r, f, out),
            ["write"] => self.write(r, out),
            ["dash", blog, "posts"] => {
                let (b, _) = self.blog_id(blog)?;
                self.new_post(r, b, Some(f), out)
            }
            ["dash", blog, "authors"] => self.add_author(r, blog, f, out),
            ["dash", blog, "authors", id, "remove"] => self.remove_author(r, blog, id, out),
            ["dash", blog, "delete"] => self.delete_blog(r, blog, out),
            ["edit", id] => self.save(r, id, f, out),
            ["edit", id, "publish"] => self.publish_draft(r, id, out),
            ["edit", id, "unpublish"] => self.unpublish(r, id, out),
            ["edit", id, "delete"] => self.delete_post(r, id, out),
            _ => Err(404),
        }
    }

    // STATIC FILES
    fn asset(&mut self, r: &mut R, name: &str, out: &mut Vec<u8>) -> Res {
        let a = crate::assets::find(name.as_bytes()).ok_or(404u16)?;
        let current = Form::parse(r.query.as_bytes()).map_or(false, |q| q.get("v") == Some(crate::assets::version()));
        let cache = if current { Cache::Immutable } else { Cache::NoStore };
        resp::whole(out, 200, a.ctype, cache, None, b"", a.bytes, !r.head_only);
        Ok(())
    }

    // READING PAGES (public; Datastar-free: plain HTML, cached by browsers
    // for Back, prefetched by speculation rules)
    fn feed(&mut self, q: Q, args: &[Val], page: u64) -> Result<(Vec<pages::FeedItem>, bool), u16> {
        let mut rows = Vec::with_capacity(PAGE_ITEMS as usize + 1);
        let mut a: Vec<Val> = args.to_vec();
        a.push(Val::Int(PAGE_ITEMS + 1));
        a.push(Val::Int((page as i64 - 1) * PAGE_ITEMS));
        self.st
            .q(q, &a, |row| {
                rows.push(pages::FeedItem {
                    slug: row.text(0).into(),
                    title: row.text(1).into(),
                    published_ms: row.int(2),
                    blog_slug: row.text(3).into(),
                    blog_title: row.text(4).into(),
                    author: row.text(5).into(),
                    handle: row.text(8).into(),
                    words: row.int(7),
                })
            })
            .map_err(db_code)?;
        let more = rows.len() > PAGE_ITEMS as usize;
        rows.truncate(PAGE_ITEMS as usize);
        Ok((rows, more))
    }

    fn home(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let page = r.page_no();
        let (rows, more) = self.feed(Q::Recent, &[], page)?;
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::home(h, s, &rows, page, more));
        Ok(())
    }

    fn blog(&mut self, r: &mut R, slug: &str, out: &mut Vec<u8>) -> Res {
        let (id, title) = self.blog_id(slug)?;
        let page = r.page_no();
        let (rows, more) = self.feed(Q::BlogPosts, &[Val::Int(id as i64)], page)?;
        let mut owner = (String::new(), String::new());
        self.st.q(Q::BlogOwner, &[Val::Int(id as i64)], |row| owner = (row.text(0).into(), row.text(1).into())).map_err(db_code)?;
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::blog(h, s, slug, &title, &owner.0, &owner.1, &rows, page, more));
        Ok(())
    }

    fn author(&mut self, r: &mut R, handle: &str, out: &mut Vec<u8>) -> Res {
        let mut u = None;
        self.st.q(Q::UserByHandle, &[Val::Text(handle.as_bytes())], |row| u = Some((row.int(0), row.text(1).to_string()))).map_err(db_code)?;
        let (id, name) = u.ok_or(404u16)?;
        let page = r.page_no();
        let (rows, more) = self.feed(Q::AuthorPosts, &[Val::Int(id)], page)?;
        if rows.is_empty() && page == 1 {
            // Only people who have published have a page.
            return Err(404);
        }
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::author(h, s, handle, &name, &rows, page, more));
        Ok(())
    }

    fn post_page(&mut self, r: &mut R, blog: &str, slug: &str, out: &mut Vec<u8>) -> Res {
        let (bid, btitle) = self.blog_id(blog)?;
        struct Meta {
            id: u64,
            title: String,
            published_ms: i64,
            author: String,
            handle: String,
            words: i64,
            published: bool,
            updated_ms: i64,
        }
        let mut m = None;
        self.st
            .q(Q::PostPage, &[Val::Int(bid as i64), Val::Text(slug.as_bytes())], |row| {
                m = Some(Meta {
                    id: row.int(0) as u64,
                    title: row.text(1).into(),
                    published_ms: row.int(2),
                    author: row.text(3).into(),
                    handle: row.text(4).into(),
                    words: row.int(5),
                    published: row.int(6) != 0,
                    updated_ms: row.int(7),
                })
            })
            .map_err(db_code)?;
        let m = m.ok_or(404u16)?;
        // Drafts: only the blog's authors (THE POLICY, ReadPost).
        let p = self.permit(r, bid, m.id, Action::ReadPost { post: m.id }).map_err(|_| 404u16)?;
        let can_edit = p.facts().role.is_some();
        let body = match self.cache.get(m.id, m.updated_ms) {
            // A draft (only its authors see it): as it is now, not cached.
            _ if !m.published => {
                let doc = self.docs.get(&mut self.st, m.id).map_err(no_code)?;
                Rc::new(crate::markdown::render(doc.text().as_bytes()))
            }
            Some(b) => b,
            None => {
                let mut md = Vec::new();
                self.st.q(Q::PostBody, &[Val::Int(m.id as i64)], |row| md = row.bytes(0).to_vec()).map_err(db_code)?;
                let b = Rc::new(crate::markdown::render(&md));
                self.cache.put(m.id, m.updated_ms, b.clone());
                b
            }
        };
        let v = pages::PostView {
            id: m.id,
            blog_slug: blog,
            blog_title: &btitle,
            title: &m.title,
            author: &m.author,
            handle: &m.handle,
            published_ms: if m.published { m.published_ms } else { m.updated_ms },
            words: m.words,
            published: m.published,
            can_edit,
            just_published: can_edit && m.published && r.query == "published=1",
        };
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::post(h, s, &v, &body));
        Ok(())
    }

    // SIGN-UP AND RECOVERY
    /// A username as typed ("@Maya", " maya ") to its stored form, if valid:
    /// a-z first, then a-z 0-9 _, 3 to 30 in all.
    fn handle_of(s: &str) -> Option<String> {
        let s = s.trim();
        let s = s.strip_prefix('@').unwrap_or(s).to_ascii_lowercase();
        let b = s.as_bytes();
        let ok = (3..=30).contains(&b.len()) && b[0].is_ascii_lowercase() && b.iter().all(|&c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_');
        if ok { Some(s) } else { None }
    }

    fn handle_taken(&mut self, handle: &str, email: &str, now: u64) -> Result<bool, u16> {
        let t = self.st.one(Q::HandleTaken, &[Val::Text(handle.as_bytes()), Val::Int(now as i64), Val::Text(email.as_bytes())]).map_err(db_code)?;
        Ok(t.is_some())
    }

    fn handle_check(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let q = Form::parse(r.query.as_bytes()).ok_or(400u16)?;
        let raw = q.get("h").unwrap_or("");
        let (note, ok): (&'static str, bool) = match Site::handle_of(raw) {
            _ if raw.trim().is_empty() => ("", true),
            None => ("3 to 30 of a-z, 0-9 and _, starting with a letter.", false),
            Some(h) => {
                if self.handle_taken(&h, "", r.now())? { ("That username is taken.", false) } else { ("Available.", true) }
            }
        };
        let mut h = H::new(String::new());
        pages::handle_note(&mut h, note, ok);
        resp::patches(out, &[("", "", &h.b)]);
        Ok(())
    }

    /// A fresh emailed link: its token (hex) and, unless in direct mode, the
    /// mail in the outbox. Inside the caller's transaction.
    fn email_link(st: &mut Store, conf: &Conf, token: &[u8; 32], email: &str, name: &str, handle: &str, purpose: i64, now: u64) -> Result<String, u16> {
        let t = hex(token);
        st.run(Q::TokenNew, &[Val::Blob(&sha256(token)), Val::Text(email.as_bytes()), Val::Text(name.as_bytes()), Val::Text(handle.as_bytes()), Val::Int(purpose), Val::Int((now + TOKEN_MS) as i64)]).map_err(db_code)?;
        if !conf.direct {
            let (subject, what) = if purpose == 1 { ("Finish signing up", "finish signing up") } else { ("Add a passkey", "add a passkey to your account") };
            let body = format!("Open this link to {what}:\n\n{}/verify?t={t}\n\nIt works once, for 30 minutes. If you did not ask for this, ignore this email: nothing happens without the link.\n", conf.origin);
            st.run(Q::Outbox, &[Val::Text(email.as_bytes()), Val::Text(subject.as_bytes()), Val::Text(body.as_bytes()), Val::Int(now as i64)]).map_err(db_code)?;
        }
        Ok(t)
    }

    fn signup(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let name = f.text("name", 80, false);
        let email = f.get("email").map(|e| e.trim().to_ascii_lowercase()).filter(|e| email_ok(e));
        let handle = f.get("handle").and_then(Site::handle_of);
        let (Some(name), Some(email), Some(handle)) = (name, email, handle) else {
            let (n, e, hd) = (f.get("name").unwrap_or(""), f.get("email").unwrap_or(""), f.get("handle").unwrap_or(""));
            let (n, e, hd) = (n.chars().take(80).collect::<String>(), e.chars().take(254).collect::<String>(), hd.chars().take(31).collect::<String>());
            self.html(r, out, 400, |h| pages::signup(h, "Check the fields: a name, a username (3 to 30 of a-z, 0-9 and _, starting with a letter) and an email address.", &n, &hd, &e));
            return Ok(());
        };
        if self.handle_taken(&handle, &email, now)? {
            self.html(r, out, 409, |h| pages::signup(h, "That username is taken. Choose another.", &name, "", &email));
            return Ok(());
        }
        let token = r.random::<32>();
        let conf = &self.conf;
        let res = txn(&mut self.st, |st| {
            // An address with an account, or too much mail to it: the same
            // answer as success (whether an address has an account is not
            // said), and no mail.
            if st.one(Q::UserByEmail, &[Val::Text(email.as_bytes())]).map_err(db_code)?.is_some() {
                return Ok(None);
            }
            let sent = st.one(Q::MailCount, &[Val::Text(email.as_bytes()), Val::Int(now as i64 - 3_600_000)]).map_err(db_code)?.unwrap_or(0);
            if sent >= MAILS_PER_HOUR {
                return Ok(None);
            }
            Site::email_link(st, conf, &token, &email, &name, &handle, 1, now).map(Some)
        })?;
        match (res, self.conf.direct) {
            (Some(t), true) => resp::redirect(out, &format!("/verify?t={t}"), b""),
            (None, true) => self.html(r, out, 409, |h| pages::signup(h, "This email may already have an account: log in instead.", &name, &handle, &email)),
            _ => self.html(r, out, 200, pages::check_email),
        }
        Ok(())
    }

    fn recover(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let email = f.get("email").map(|e| e.trim().to_ascii_lowercase()).filter(|e| email_ok(e)).ok_or(400u16)?;
        let token = r.random::<32>();
        let conf = &self.conf;
        txn(&mut self.st, |st| {
            let mut user = None;
            st.q(Q::UserByEmail, &[Val::Text(email.as_bytes())], |row| user = Some(row.int(0))).map_err(db_code)?;
            let Some(uid) = user else { return Ok(()) };
            let sent = st.one(Q::MailCount, &[Val::Text(email.as_bytes()), Val::Int(now as i64 - 3_600_000)]).map_err(db_code)?.unwrap_or(0);
            if sent >= MAILS_PER_HOUR {
                return Ok(());
            }
            let mut nh = (String::new(), String::new());
            st.q(Q::UserInfo, &[Val::Int(uid)], |row| nh = (row.text(0).into(), row.text(1).into())).map_err(db_code)?;
            Site::email_link(st, conf, &token, &email, &nh.0, &nh.1, 2, now).map(|_| ())
        })?;
        self.html(r, out, 200, pages::check_email);
        Ok(())
    }

    /// An emailed link's token (64 hex) and what it is for, if still valid:
    /// (hash, email, name, handle, purpose).
    fn token(&mut self, t: &str, now: u64) -> Result<([u8; 32], String, String, String, i64), u16> {
        let raw: [u8; 32] = if t.len() == 64 { unhex(t.as_bytes()).and_then(|v| v.try_into().ok()).ok_or(404u16)? } else { return Err(404) };
        let h = sha256(&raw);
        let mut out = None;
        self.st.q(Q::TokenGet, &[Val::Blob(&h), Val::Int(now as i64)], |row| out = Some((row.text(0).into(), row.text(1).into(), row.text(2).into(), row.int(3)))).map_err(db_code)?;
        let (e, n, hd, p) = out.ok_or(404u16)?;
        Ok((h, e, n, hd, p))
    }

    fn verify(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let q = Form::parse(r.query.as_bytes()).ok_or(404u16)?;
        let t = q.get("t").ok_or(404u16)?.to_string();
        let (_, email, _, _, purpose) = self.token(&t, r.now())?;
        self.html(r, out, 200, |h| pages::verify(h, &t, &email, purpose == 2));
        Ok(())
    }

    // PASSKEYS
    fn register_options(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let (th, email, name, _, _) = self.token(f.get("t").ok_or(400u16)?, now).map_err(|_| 403u16)?;
        let chal = r.random::<32>();
        self.st.run(Q::ChallengeNew, &[Val::Blob(&sha256(&chal)), Val::Int(1), Val::Blob(&th), Val::Int((now + CHALLENGE_MS) as i64)]).map_err(db_code)?;
        // The user handle given to the authenticator: random (we find the
        // account by the credential id, never by this).
        let uid = r.random::<16>();
        let mut j = String::with_capacity(300);
        j.push_str("{\"challenge\":\"");
        j.push_str(&wa::b64url_encode(&chal));
        j.push_str("\",\"rpId\":");
        json_str(&mut j, &self.conf.rp_id);
        j.push_str(",\"rpName\":\"slopstack\",\"userId\":\"");
        j.push_str(&wa::b64url_encode(&uid));
        j.push_str("\",\"userName\":");
        json_str(&mut j, &email);
        j.push_str(",\"userDisplay\":");
        json_str(&mut j, &name);
        j.push('}');
        Site::json(out, j.as_bytes());
        Ok(())
    }

    fn expected(&self) -> wa::Expected {
        wa::Expected { origin: self.conf.origin.as_bytes().to_vec(), rp_hash: self.conf.rp_hash.to_vec() }
    }

    fn register(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let (th, email, name, handle, purpose) = self.token(f.get("t").ok_or(400u16)?, now).map_err(|_| 403u16)?;
        let cd_raw = wa::b64url(f.get("cd").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let att = wa::b64url(f.get("att").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let cd = wa::client_data(&cd_raw).ok_or(400u16)?;
        let ad = wa::attestation_auth_data(&att).ok_or(400u16)?;
        let p = wa::auth_data(&ad).ok_or(400u16)?;
        let key = p.public_key.ok_or(400u16)?;
        // THE REGISTRATION LAW (proved).
        let passed = wa::checked(wa::register_decision(&self.expected(), &cd, &p.auth)).ok_or(403u16)?;
        let chal_hash = sha256(&cd.challenge);
        let token = r.random::<32>();
        let count = p.auth.count as i64;
        txn(&mut self.st, |st| {
            // The challenge: ours, for registering, for this link, unexpired,
            // and now used; the link too.
            if st.run(Q::ChallengeTake, &[Val::Blob(&chal_hash), Val::Int(1), Val::Int(now as i64), Val::Blob(&th)]).map_err(db_code)? != 1 {
                return Err(403);
            }
            if st.run(Q::TokenUse, &[Val::Blob(&th), Val::Int(now as i64)]).map_err(db_code)? != 1 {
                return Err(403);
            }
            let uid = if purpose == 1 {
                st.run(Q::UserNew, &[Val::Text(email.as_bytes()), Val::Text(name.as_bytes()), Val::Text(handle.as_bytes()), Val::Int(now as i64)]).map_err(|_| 403u16)?;
                st.db.last_rowid()
            } else {
                st.one(Q::UserByEmail, &[Val::Text(email.as_bytes())]).map_err(db_code)?.ok_or(403u16)?
            };
            st.run(Q::CredNew, &[Val::Blob(&p.cred_id), Val::Int(uid), Val::Blob(&key), Val::Int(count), Val::Int(now as i64)]).map_err(|_| 403u16)?;
            st.session_for(&passed, uid as u64, &token, now).map_err(db_code)
        })?;
        Site::text(out, 200, b"/dash", &resp::cookie(Some(&hex(&token)), self.conf.secure));
        Ok(())
    }

    fn login_options(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let minute = now / 60_000;
        if minute != self.minute {
            self.minute = minute;
            self.challenges = 0;
        }
        if self.challenges >= CHALLENGES_PER_MINUTE {
            return Err(503);
        }
        self.challenges += 1;
        let chal = r.random::<32>();
        self.st.run(Q::ChallengeNew, &[Val::Blob(&sha256(&chal)), Val::Int(2), Val::Null, Val::Int((now + CHALLENGE_MS) as i64)]).map_err(db_code)?;
        let mut j = String::with_capacity(120);
        j.push_str("{\"challenge\":\"");
        j.push_str(&wa::b64url_encode(&chal));
        j.push_str("\",\"rpId\":");
        json_str(&mut j, &self.conf.rp_id);
        j.push('}');
        Site::json(out, j.as_bytes());
        Ok(())
    }

    fn login(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let id = wa::b64url(f.get("id").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let cd_raw = wa::b64url(f.get("cd").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let ad = wa::b64url(f.get("ad").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let sig = wa::b64url(f.get("sig").ok_or(400u16)?.as_bytes()).ok_or(400u16)?;
        let cd = wa::client_data(&cd_raw).ok_or(400u16)?;
        let p = wa::auth_data(&ad).ok_or(400u16)?;
        if p.public_key.is_some() {
            return Err(400);
        }
        let mut cred = None;
        self.st
            .q(Q::CredGet, &[Val::Blob(&id)], |row| {
                let k: Option<[u8; 64]> = row.bytes(1).try_into().ok();
                cred = k.map(|k| (row.int(0) as u64, k, row.int(2)));
            })
            .map_err(db_code)?;
        let (uid, key, stored) = cred.ok_or(403u16)?;
        let stored32 = u32::try_from(stored).map_err(|_| 503u16)?;
        let mut msg = ad.clone();
        msg.extend_from_slice(&sha256(&cd_raw));
        let sig_ok = p256_verify(&key, &msg, &sig);
        // THE LOGIN LAW (proved).
        let passed = wa::checked(wa::login_decision(&self.expected(), &cd, &p.auth, sig_ok, stored32)).ok_or(403u16)?;
        let chal_hash = sha256(&cd.challenge);
        let token = r.random::<32>();
        txn(&mut self.st, |st| {
            if st.run(Q::ChallengeTake, &[Val::Blob(&chal_hash), Val::Int(2), Val::Int(now as i64), Val::Null]).map_err(db_code)? != 1 {
                return Err(403);
            }
            // The counter moves only from the value the decision saw.
            if st.run(Q::CredCount, &[Val::Blob(&id), Val::Int(p.auth.count as i64), Val::Int(stored)]).map_err(db_code)? != 1 {
                return Err(403);
            }
            st.session_for(&passed, uid, &token, now).map_err(db_code)
        })?;
        Site::text(out, 200, b"/dash", &resp::cookie(Some(&hex(&token)), self.conf.secure));
        Ok(())
    }

    // DASHBOARD
    fn signed(&self, r: &R, out: &mut Vec<u8>) -> bool {
        if !r.signed_in() {
            resp::redirect(out, "/login", b"");
            return false;
        }
        true
    }

    fn my_blogs(&mut self, uid: u64) -> Result<Vec<pages::BlogCard>, u16> {
        let mut v = Vec::new();
        self.st
            .q(Q::MyBlogs, &[Val::Int(uid as i64)], |row| {
                v.push(pages::BlogCard { id: row.int(0), slug: row.text(1).into(), title: row.text(2).into(), owner: row.int(3) == 1, posts: row.int(4) })
            })
            .map_err(db_code)?;
        Ok(v)
    }

    fn dash(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        if !self.signed(r, out) {
            return Ok(());
        }
        let blogs = self.my_blogs(r.uid())?;
        self.html(r, out, 200, |h| pages::dash(h, &blogs));
        Ok(())
    }

    fn authors(&mut self, blog: u64) -> Result<Vec<pages::Author>, u16> {
        let mut v = Vec::new();
        self.st
            .q(Q::Authors, &[Val::Int(blog as i64)], |row| v.push(pages::Author { id: row.int(0), name: row.text(1).into(), email: row.text(2).into(), owner: row.int(3) == 1 }))
            .map_err(db_code)?;
        Ok(v)
    }

    fn dash_blog(&mut self, r: &mut R, slug: &str, note: &str, out: &mut Vec<u8>) -> Res {
        if !self.signed(r, out) {
            return Ok(());
        }
        let (bid, title) = self.blog_id(slug)?;
        let p = self.permit(r, bid, 0, Action::ReadBlogAdmin { blog: bid })?;
        let is_owner = matches!(p.facts().role, Some(crate::spec_authz::Role::Owner));
        let mut posts = Vec::new();
        self.st
            .q(Q::PostsAdmin, &[Val::Int(bid as i64), Val::Int(1000), Val::Int(0)], |row| {
                posts.push(pages::PostRow { id: row.int(0), slug: row.text(1).into(), title: row.text(2).into(), published: row.int(3) != 0, updated_ms: row.int(4) })
            })
            .map_err(db_code)?;
        let authors = self.authors(bid)?;
        self.html(r, out, 200, |h| pages::dash_blog(h, slug, &title, is_owner, &posts, &authors, note));
        Ok(())
    }

    fn new_blog(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let title = f.text("title", 200, false).ok_or(400u16)?;
        let wanted = match f.get("slug").map(str::trim) {
            Some(s) if !s.is_empty() => {
                if s.len() > 64 || !s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
                    return Err(400);
                }
                Some(s.to_string())
            }
            _ => None,
        };
        let base = wanted.clone().unwrap_or_else(|| {
            let s = slugify(&title, 56);
            if s.is_empty() { "blog".into() } else { s }
        });
        let p = self.permit(r, 0, 0, Action::CreateBlog)?;
        let (now, uid) = (r.now(), r.uid());
        let slug = self
            .st
            .write(&p, now, true, |st, _| {
                // A chosen address must be free; a made-up one gets a number.
                for i in 1..=100u32 {
                    let s = if i == 1 { base.clone() } else { format!("{base}-{i}") };
                    if st.one(Q::SlugBlogTaken, &[Val::Text(s.as_bytes())])?.is_some() {
                        if wanted.is_some() {
                            return Err(No::Conflict);
                        }
                        continue;
                    }
                    st.run(Q::BlogNew, &[Val::Text(s.as_bytes()), Val::Text(title.as_bytes()), Val::Int(now as i64)])?;
                    let bid = st.db.last_rowid();
                    st.run(Q::MemberAdd, &[Val::Int(bid), Val::Int(uid as i64), Val::Int(1)])?;
                    return Ok(s);
                }
                Err(No::Conflict)
            })
            .map_err(no_code)?;
        resp::redirect(out, &format!("/dash/{slug}"), b"");
        Ok(())
    }

    fn delete_blog(&mut self, r: &mut R, slug: &str, out: &mut Vec<u8>) -> Res {
        let (bid, _) = self.blog_id(slug)?;
        let p = self.permit(r, bid, 0, Action::DeleteBlog { blog: bid })?;
        self.st.write(&p, r.now(), true, |st, _| Ok(st.run(Q::BlogDel, &[Val::Int(bid as i64)])?)).map_err(no_code)?;
        resp::redirect(out, "/dash", b"");
        Ok(())
    }

    /// The Write button: a new post in your blog (the only one), or the
    /// dashboard to choose one (or to start one).
    fn write(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let blogs = self.my_blogs(r.uid())?;
        if blogs.len() == 1 {
            return self.new_post(r, blogs[0].id as u64, None, out);
        }
        resp::redirect(out, "/dash", b"");
        Ok(())
    }

    /// A new draft: titled "Untitled" unless the form names it; its address
    /// is a placeholder (draft-…) until its first publish takes one from
    /// the title, unless the form chose one.
    fn new_post(&mut self, r: &mut R, blog: u64, f: Option<&Form>, out: &mut Vec<u8>) -> Res {
        let title = match f.and_then(|f| f.get("title")) {
            Some(t) if !t.trim().is_empty() => f.and_then(|f| f.text("title", 200, false)).ok_or(400u16)?,
            _ => "Untitled".to_string(),
        };
        let slug = match f.and_then(|f| f.get("slug")).map(str::trim) {
            Some(s) if !s.is_empty() => {
                if s.len() > 80 || s.starts_with("draft-") || !s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
                    return Err(400);
                }
                s.to_string()
            }
            _ => format!("draft-{}", hex(&r.random::<6>())),
        };
        let p = self.permit(r, blog, 0, Action::CreatePost { blog })?;
        let (now, uid) = (r.now(), r.uid());
        let id = self
            .st
            .write(&p, now, true, |st, _| {
                st.run(Q::PostNew, &[Val::Int(blog as i64), Val::Text(slug.as_bytes()), Val::Text(title.as_bytes()), Val::Int(uid as i64), Val::Int(now as i64)])?;
                Ok(st.db.last_rowid())
            })
            .map_err(no_code)?;
        resp::redirect(out, &format!("/edit/{id}"), b"");
        Ok(())
    }

    // AUTHORS (Datastar: the list and the note are patched in place)
    fn add_author(&mut self, r: &mut R, slug: &str, f: &Form, out: &mut Vec<u8>) -> Res {
        let (bid, _) = self.blog_id(slug)?;
        let email = f.get("email").map(|e| e.trim().to_ascii_lowercase()).unwrap_or_default();
        let user = if email_ok(&email) { self.st.one(Q::UserByEmail, &[Val::Text(email.as_bytes())]).map_err(db_code)? } else { None };
        let note = match user {
            None => "No account with that email. They need to sign up first.",
            Some(u) => {
                let p = self.permit(r, bid, 0, Action::AddAuthor { blog: bid, user: u as u64 })?;
                match self.st.write(&p, r.now(), true, |st, _| Ok(st.run(Q::MemberAdd, &[Val::Int(bid as i64), Val::Int(u), Val::Int(2)])?)) {
                    Ok(_) => "Added.",
                    Err(No::Conflict) => "Already an author here.",
                    Err(e) => return Err(no_code(e)),
                }
            }
        };
        // Only the owner may see the rest (or learn whether an address has
        // an account): checked above for a found user; here for all.
        self.permit(r, bid, 0, Action::EditBlog { blog: bid })?;
        if r.ds {
            let authors = self.authors(bid)?;
            let mut h = H::new(String::new());
            pages::people(&mut h, slug, true, &authors);
            let mut n = H::new(String::new());
            pages::note(&mut n, note);
            resp::patches(out, &[("", "", &h.b), ("", "", &n.b)]);
            return Ok(());
        }
        self.dash_blog(r, slug, note, out)
    }

    fn remove_author(&mut self, r: &mut R, slug: &str, id: &str, out: &mut Vec<u8>) -> Res {
        let (bid, _) = self.blog_id(slug)?;
        let uid: u64 = id.parse().map_err(|_| 404u16)?;
        let p = self.permit(r, bid, 0, Action::RemoveAuthor { blog: bid, user: uid })?;
        self.st.write(&p, r.now(), true, |st, _| Ok(st.run(Q::MemberDel, &[Val::Int(bid as i64), Val::Int(uid as i64)])?)).map_err(no_code)?;
        if r.ds {
            let authors = self.authors(bid)?;
            let mut h = H::new(String::new());
            pages::people(&mut h, slug, true, &authors);
            resp::patches(out, &[("", "", &h.b)]);
            return Ok(());
        }
        resp::redirect(out, &format!("/dash/{slug}"), b"");
        Ok(())
    }

    // POSTS
    fn post_id(id: &str) -> Result<u64, u16> {
        if id.is_empty() || id.len() > 15 || !id.bytes().all(|c| c.is_ascii_digit()) {
            return Err(404);
        }
        id.parse().map_err(|_| 404)
    }

    fn edit(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        if !self.signed(r, out) {
            return Ok(());
        }
        let id = Site::post_id(id)?;
        self.permit(r, 0, id, Action::EditPost { post: id })?;
        let mut e = None;
        self.st
            .q(Q::EditGet, &[Val::Int(id as i64)], |row| e = Some((row.text(7).to_string(), row.text(1).to_string(), row.int(2) != 0, row.text(3).to_string(), row.text(5).to_string())))
            .map_err(db_code)?;
        let (title, slug, published, blog_slug, _) = e.ok_or(404u16)?;
        // A replica number for this page's editor: the ids it makes.
        let uid = r.uid();
        let rep = self.st.one(Q::RepNew, &[Val::Int(id as i64)]).map_err(db_code)?.ok_or(404u16)?;
        self.st.run(Q::RepAdd, &[Val::Int(id as i64), Val::Int(rep), Val::Int(uid as i64)]).map_err(db_code)?;
        // The text in the page (for reading while the editor loads, and the
        // form without JavaScript), unless it is long: then the editor
        // loads it, and the form cannot replace it.
        let text = self.docs.get(&mut self.st, id).map_err(no_code)?.text();
        let big = text.len() > EMBED_MAX;
        let body = if big { "" } else { text.as_str() };
        let v = pages::EditView { id, rep: rep as u32, title: &title, slug: &slug, published, blog_slug: &blog_slug, body, big };
        self.html(r, out, 200, |h| pages::edit(h, &v));
        Ok(())
    }

    fn save(&mut self, r: &mut R, id: &str, f: &Form, out: &mut Vec<u8>) -> Res {
        let id = Site::post_id(id)?;
        let title = f.text("title", 200, false).ok_or(400u16)?;
        // The text: from the editor's document; a form sent without
        // JavaScript carries it whole (it replaces the document's).
        let body = match f.get("body") {
            Some(b) => {
                if b.len() > TEXT_MAX || b.chars().any(|c| c.is_control() && c != '\n' && c != '\r' && c != '\t') {
                    return Err(if b.len() > TEXT_MAX { 413 } else { 400 });
                }
                Some(b.replace("\r\n", "\n"))
            }
            None => None,
        };
        let publish = f.get("action") == Some("publish");
        let action = if publish { Action::PublishPost { post: id } } else { Action::EditPost { post: id } };
        let p = self.permit(r, 0, id, action)?;
        let now = r.now();
        let blog = p.facts().blog;
        let docs = &mut self.docs;
        let dest = self
            .st
            .write(&p, now, true, |st, _| {
                if let Some(b) = &body {
                    docs.set_text(st, id, b)?;
                }
                if !publish {
                    st.run(Q::DraftSave, &[Val::Int(id as i64), Val::Text(title.as_bytes()), Val::Int(now as i64)])?;
                    return Ok(format!("/edit/{id}"));
                }
                let body = docs.get(st, id)?.text();
                let words = crate::markdown::words(body.as_bytes());
                // The address: fixed at the first publish (links stay).
                let mut cur = None;
                st.q(Q::EditGet, &[Val::Int(id as i64)], |row| cur = Some((row.text(1).to_string(), row.text(3).to_string(), row.int(6) != 0)))?;
                let (mut slug, blog_slug, never) = cur.ok_or(No::Denied)?;
                if never && slug.starts_with("draft-") {
                    let base = slugify(&title, 72);
                    let base = if base.is_empty() { "post".to_string() } else { base };
                    slug = String::new();
                    for i in 1..=100u32 {
                        let s = if i == 1 { base.clone() } else { format!("{base}-{i}") };
                        if st.one(Q::SlugTaken, &[Val::Int(blog as i64), Val::Text(s.as_bytes())])?.is_none() {
                            slug = s;
                            break;
                        }
                    }
                    if slug.is_empty() {
                        return Err(No::Conflict);
                    }
                }
                st.run(Q::PostPublish, &[Val::Int(id as i64), Val::Text(title.as_bytes()), Val::Text(body.as_bytes()), Val::Int(words as i64), Val::Int(now as i64), Val::Text(slug.as_bytes())])?;
                Ok(format!("/b/{blog_slug}/{slug}?published=1"))
            })
            .map_err(no_code)?;
        resp::redirect(out, &dest, b"");
        Ok(())
    }

    /// Publish the saved draft as it is.
    fn publish_draft(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let pid = Site::post_id(id)?;
        let mut d = None;
        self.st.q(Q::EditGet, &[Val::Int(pid as i64)], |row| d = Some(row.text(7).to_string())).map_err(db_code)?;
        let title = d.ok_or(404u16)?;
        let mut f = Vec::new();
        for (k, v) in [("title", title.as_str()), ("action", "publish")] {
            if !f.is_empty() {
                f.push(b'&');
            }
            f.extend_from_slice(k.as_bytes());
            f.push(b'=');
            f.extend_from_slice(crate::form::encode(v).as_bytes());
        }
        let form = Form::parse(&f).ok_or(400u16)?;
        self.save(r, id, &form, out)
    }

    fn unpublish(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let id = Site::post_id(id)?;
        let p = self.permit(r, 0, id, Action::PublishPost { post: id })?;
        let now = r.now();
        self.st.write(&p, now, true, |st, _| Ok(st.run(Q::PostUnpublish, &[Val::Int(id as i64), Val::Int(now as i64)])?)).map_err(no_code)?;
        resp::redirect(out, &format!("/edit/{id}"), b"");
        Ok(())
    }

    fn delete_post(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let id = Site::post_id(id)?;
        let p = self.permit(r, 0, id, Action::DeletePost { post: id })?;
        let blog = p.facts().blog;
        let mut slug = String::new();
        self.st.q(Q::BlogSlug, &[Val::Int(blog as i64)], |row| slug = row.text(0).into()).map_err(db_code)?;
        self.st.write(&p, r.now(), true, |st, _| Ok(st.run(Q::PostDel, &[Val::Int(id as i64)])?)).map_err(no_code)?;
        self.cache.map.remove(&id).map(|(_, m)| self.cache.bytes -= m.len());
        self.docs.forget(id);
        resp::redirect(out, &format!("/dash/{slug}"), b"");
        Ok(())
    }

    // THE EDITOR'S SYNC (local-first: the browser holds the document and
    // sends its operations; the answer brings everyone else's). Body:
    // since (u64: the last stored batch the editor has), rep (u32: its
    // replica number), then operations (src/crdt.rs wire format).
    fn sync(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let id = Site::post_id(id)?;
        let b = r.req.body;
        if b.len() < 12 {
            return Err(400);
        }
        let since = u64::from_le_bytes(b[..8].try_into().expect("8")) as i64;
        let rep = u32::from_le_bytes(b[8..12].try_into().expect("4"));
        let ops = &b[12..];
        let p = self.permit(r, 0, id, Action::EditPost { post: id })?;
        let (uid, now) = (r.uid(), r.now());
        if !ops.is_empty() {
            // The replica number must be one this user was given here.
            let owner = self.st.one(Q::RepOwner, &[Val::Int(id as i64), Val::Int(rep as i64)]).map_err(db_code)?;
            if owner != Some(uid as i64) {
                return Err(403);
            }
            let docs = &mut self.docs;
            self.st
                .write(&p, now, false, |st, _| {
                    docs.store(st, id, rep, ops)?;
                    st.run(Q::Edited, &[Val::Int(id as i64), Val::Int(now as i64)])?;
                    Ok(())
                })
                .map_err(no_code)?;
        }
        let mut body = Vec::new();
        self.docs.reply(&mut self.st, id, since, &mut body).map_err(no_code)?;
        resp::whole(out, 200, "application/octet-stream", Cache::NoStore, None, b"", &body, true);
        Ok(())
    }

    /// Expired challenges, links and sessions: removed once a minute.
    fn sweep(&mut self, now: u64) {
        if now < self.swept_ms + 60_000 {
            return;
        }
        self.swept_ms = now;
        for q in [Q::Sweep, Q::SweepTokens, Q::SweepSessions] {
            let _ = self.st.run(q, &[Val::Int(now as i64)]);
        }
    }
}

// TRANSACTIONS (the account flows: nobody is signed in yet, so these
// are not Store::write, which is for permits).
fn txn<T>(st: &mut Store, f: impl FnOnce(&mut Store) -> Result<T, u16>) -> Result<T, u16> {
    st.run(Q::Begin, &[]).map_err(db_code)?;
    match f(st) {
        Ok(v) => {
            st.run(Q::Commit, &[]).map_err(db_code)?;
            Ok(v)
        }
        Err(e) => {
            let _ = st.run(Q::Rollback, &[]);
            Err(e)
        }
    }
}

/// A JSON string (quotes, escapes).
fn json_str(j: &mut String, s: &str) {
    j.push('"');
    for c in s.chars() {
        match c {
            '"' => j.push_str("\\\""),
            '\\' => j.push_str("\\\\"),
            c if (c as u32) < 0x20 || c == '<' || c == '>' || c == '&' => j.push_str(&format!("\\u{:04x}", c as u32)),
            c => j.push(c),
        }
    }
    j.push('"');
}

impl App for Site {
    fn handle(&mut self, req: &Request, cx: &mut Ctx, out: &mut Vec<u8>) -> bool {
        let now = cx.now_ms;
        self.sweep(now);
        let target = std::str::from_utf8(req.target).unwrap_or("");
        let (path, query) = match target.find('?') {
            Some(i) => (&target[..i], &target[i + 1..]),
            None => (target, ""),
        };
        // A header that decides who you are or where you came from, twice:
        // which one counts is not agreed on, so neither does.
        if req.count(b"cookie") > 1 || req.count(b"sec-fetch-site") > 1 {
            let mut r = R { req, cx, path: "/", query: "", who: None, head_only: false, ds: false };
            self.error_page(&mut r, out, 400);
            return true;
        }
        let who = self.who(req, now);
        let ds = req.header(b"datastar-request").is_some();
        let mut r = R { req, cx, path, query, who, head_only: req.method == b"HEAD", ds };
        if !path.starts_with('/') || !path.is_ascii() {
            self.error_page(&mut r, out, 400);
            return true;
        }
        if let Err(code) = self.route(&mut r, out) {
            self.error_page(&mut r, out, code);
        }
        true
    }
}
