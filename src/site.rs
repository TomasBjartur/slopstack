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
use crate::notify::{Changes, Kind};
use crate::pages::{self, H};
use crate::resp::{self, hex, unhex, Cache};
use crate::server::{App, Ctx, Request};
use crate::spec_authz::{Action, Facts};
use crate::sys::crypto::{p256_verify, sha256};
use crate::sys::sqlite::{DbErr, Val};
use crate::webauthn as wa;
use crate::hash::{map, Map};
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
/// Comments: threads a page, replies shown with them, characters each.
const THREADS_PAGE: i64 = 20;
const REPLIES_MAX: i64 = 1000;
const COMMENT_MAX: usize = 10_000;
/// Rendered comments kept (a comment is at most 10,000 characters: at
/// most ~2 GB in the worst case, ~100 MB for typical comments of 1 KB).
const COMMENT_CACHE: usize = 100_000;
/// An image (the editor resizes to fit): 2 MiB.
const IMAGE_MAX: usize = 2 << 20;
/// Editor page loads a user may make a minute (in each worker).
const OPENS_PER_MINUTE: u32 = 120;
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
    /// How long an editor's request for others' changes waits for them
    /// (BLOG_WAIT_MS; tests shorten it).
    pub wait_ms: u64,
}

impl Conf {
    pub fn new(origin: &str, rp_id: &str, direct: bool) -> Conf {
        Conf { origin: origin.to_string(), rp_id: rp_id.to_string(), rp_hash: sha256(rp_id.as_bytes()), direct, secure: origin.starts_with("https://"), wait_ms: WAIT_MS }
    }

    pub fn from_env(port: u16) -> Conf {
        let origin = std::env::var("BLOG_ORIGIN").unwrap_or_else(|_| format!("http://localhost:{port}"));
        let rp_id = std::env::var("BLOG_RP_ID").unwrap_or_else(|_| "localhost".into());
        let mut c = Conf::new(&origin, &rp_id, std::env::var("BLOG_SIGNUP_DIRECT").as_deref() == Ok("1"));
        if let Some(ms) = std::env::var("BLOG_WAIT_MS").ok().and_then(|v| v.parse().ok()) {
            c.wait_ms = ms;
        }
        c
    }
}

/// Feed pages' main parts by path and query, as of a feed generation.
/// Bounded: past FEEDS_MAX entries or FEEDS_BYTES, emptied.
struct FeedCache {
    map: Map<String, (i64, String, Rc<Vec<u8>>)>,
    bytes: usize,
}

/// An editor waiting for others' changes (long polling) is answered when
/// there are some, or after this with none (under the proxies' and the
/// server's own timeouts: limits::PARK_TIMEOUT_MS).
const WAIT_MS: u64 = 25_000;
/// Requests waiting at once, in one worker, and by one user: past these,
/// answered at once (the editor then waits a while before asking again).
const WAITS_MAX: usize = 20_000;
const WAITS_PER_USER: usize = 32;
/// The next live-comments request goes after this: at once when the
/// server waits for comments, after a pause when it could not.
const LIVE_NEXT: &str = "100ms";
const LIVE_PAUSE: &str = "10s";

/// A parked request: answered when its post changes (or at `until`).
struct Wait {
    conn: u64,
    post: u64,
    uid: u64,
    what: Waiting,
    /// The post's change counter when last looked at (src/notify.rs).
    seen: u32,
    /// Answered by then (monotonic ms) in any case.
    until: u64,
}

enum Waiting {
    /// The editor's sync: batches after since, but for its own (rep).
    Doc { since: i64, rep: u32 },
    /// A reader's live comments: those after `after`.
    Comments { after: u64, n: u64 },
}

impl Waiting {
    fn kind(&self) -> Kind {
        match self {
            Waiting::Doc { .. } => Kind::Doc,
            Waiting::Comments { .. } => Kind::Comments,
        }
    }
}

const FEEDS_MAX: usize = 20_000;
const FEEDS_BYTES: usize = 64 << 20;

impl FeedCache {
    fn put(&mut self, key: String, gen: i64, title: String, main: Rc<Vec<u8>>) {
        if self.map.len() >= FEEDS_MAX || self.bytes + main.len() > FEEDS_BYTES {
            self.map.clear();
            self.bytes = 0;
        }
        self.bytes += main.len();
        if let Some((_, _, old)) = self.map.insert(key, (gen, title, main)) {
            self.bytes -= old.len();
        }
    }
}

/// Rendered posts by id, as of a version (a published post's updated_ms;
/// a draft's document seq), with their word count. Bounded by bytes: past
/// the budget, entries are dropped until it fits.
struct PostCache {
    map: Map<u64, (i64, Rc<Markup>, i64)>,
    bytes: usize,
    budget: usize,
}

impl PostCache {
    fn new(budget: usize) -> PostCache {
        PostCache { map: map(), bytes: 0, budget }
    }

    fn get(&self, id: u64, version: i64) -> Option<(Rc<Markup>, i64)> {
        match self.map.get(&id) {
            Some((v, m, w)) if *v == version => Some((m.clone(), *w)),
            _ => None,
        }
    }

    fn put(&mut self, id: u64, version: i64, m: Rc<Markup>, words: i64) {
        let n = m.len();
        if n > self.budget / 4 {
            return;
        }
        if let Some((_, old, _)) = self.map.remove(&id) {
            self.bytes -= old.len();
        }
        while self.bytes + n > self.budget {
            let k = *self.map.keys().next().expect("bytes > 0 means entries");
            let (_, old, _) = self.map.remove(&k).expect("key just seen");
            self.bytes -= old.len();
        }
        self.bytes += n;
        self.map.insert(id, (version, m, words));
    }
}

pub struct Site {
    pub st: Store,
    docs: crate::docs::Docs,
    feeds: FeedCache,
    /// BLOG_STMT_STATS=1: print statements SQLite prepared again (profiling).
    stmt_stats: bool,
    requests: u64,
    /// Rendered comments by id (COMMENT_CACHE of them at most).
    comments_md: Map<u64, Rc<Markup>>,
    conf: Conf,
    cache: PostCache,
    /// Drafts and previews (the editor's current text), by document seq.
    drafts: PostCache,
    /// Editor page loads this minute, by user (OPENS_PER_MINUTE).
    opens: Map<u64, (u64, u32)>,
    /// Login challenges made this minute (CHALLENGES_PER_MINUTE).
    minute: u64,
    challenges: u32,
    swept_ms: u64,
    /// What changed, across worker processes; and the requests waiting for it.
    changes: Changes,
    waits: Vec<Wait>,
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
    pub fn new(st: Store, conf: Conf, changes: Changes) -> Site {
        Site { st, docs: crate::docs::Docs::new(), feeds: FeedCache { map: map(), bytes: 0 }, stmt_stats: std::env::var("BLOG_STMT_STATS").as_deref() == Ok("1"), requests: 0, comments_md: map(), conf, cache: PostCache::new(CACHE_BYTES), drafts: PostCache::new(CACHE_BYTES / 4), opens: map(), minute: 0, challenges: 0, swept_ms: 0, changes, waits: Vec::new() }
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
        if !matches!(c.get(i + 68), None | Some(b';') | Some(b' ')) {
            return None;
        }
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
            // An image: bytes, not a form.
            if let ["upload", id] = parts.as_slice() {
                return self.upload(r, id, out);
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
                let next = safe_next(Form::parse(r.query.as_bytes()).as_ref().and_then(|q| q.get("next")));
                self.html(r, out, 200, |h| pages::signup(h, "", "", "", "", next.as_deref().unwrap_or("")));
                Ok(())
            }
            ["login"] | ["recover"] => {
                // Signed in already: on to where it was going.
                if r.signed_in() {
                    let next = safe_next(Form::parse(r.query.as_bytes()).as_ref().and_then(|q| q.get("next"))).unwrap_or_else(|| "/dash".into());
                    resp::redirect(out, &next, b"");
                    return Ok(());
                }
                // (/recover: the same page, its "lost your passkey" part open.)
                let lost = parts[0] == "recover";
                self.html(r, out, 200, |h| pages::login(h, false, lost));
                Ok(())
            }
            ["handle"] => self.handle_check(r, out),
            ["verify"] => self.verify(r, out),
            ["dash"] => self.dash(r, out),
            ["dash", blog] => self.dash_blog(r, blog, "", out),
            ["edit", id] => self.edit(r, id, out),
            ["edit", id, "preview"] => self.preview(r, id, out),
            ["comments", id] => self.more(r, id, out),
            ["live", id] => self.live(r, id, out),
            ["reply", id] => self.reply(r, id, out),
            ["img", key] => self.image(r, key, out),
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
            ["comment", id] => self.comment(r, id, f, out),
            ["comment", id, "delete"] => self.delete_comment(r, id, out),
            ["like", id] => self.like(r, id, f, out),
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

    /// A feed page's main part: from the cache if the feed generation is
    /// the one it was made at, else made (make answers the page's title and
    /// fills the part; its errors, such as 404, are not cached).
    fn feed_page(&mut self, r: &mut R, masthead: bool, out: &mut Vec<u8>, make: impl FnOnce(&mut Site, &mut H) -> Result<String, u16>) -> Res {
        let gen = self.st.one(Q::Gen, &[]).map_err(db_code)?.unwrap_or(0);
        let key = format!("{}?{}", r.path, r.query);
        let (title, main) = match self.feeds.map.get(&key) {
            Some((g, t, m)) if *g == gen => (t.clone(), m.clone()),
            _ => {
                let mut h = H::new(String::new());
                let t = make(self, &mut h)?;
                let m = Rc::new(h.b);
                self.feeds.put(key, gen, t.clone(), m.clone());
                (t, m)
            }
        };
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::feed_page(h, &title, s, masthead, &main));
        Ok(())
    }

    fn home(&mut self, r: &mut R, out: &mut Vec<u8>) -> Res {
        let page = r.page_no();
        self.feed_page(r, page == 1, out, |site, h| {
            let (rows, more) = site.feed(Q::Recent, &[], page)?;
            pages::home_main(h, &rows, page, more);
            Ok("Recent posts".into())
        })
    }

    fn blog(&mut self, r: &mut R, slug: &str, out: &mut Vec<u8>) -> Res {
        let page = r.page_no();
        self.feed_page(r, false, out, |site, h| {
            let (id, title) = site.blog_id(slug)?;
            let (rows, more) = site.feed(Q::BlogPosts, &[Val::Int(id as i64)], page)?;
            let mut owner = (String::new(), String::new());
            site.st.q(Q::BlogOwner, &[Val::Int(id as i64)], |row| owner = (row.text(0).into(), row.text(1).into())).map_err(db_code)?;
            pages::blog_main(h, slug, &title, &owner.0, &owner.1, &rows, page, more);
            Ok(title)
        })
    }

    fn author(&mut self, r: &mut R, handle: &str, out: &mut Vec<u8>) -> Res {
        let page = r.page_no();
        self.feed_page(r, false, out, |site, h| {
            let mut u = None;
            site.st.q(Q::UserByHandle, &[Val::Text(handle.as_bytes())], |row| u = Some((row.int(0), row.text(1).to_string()))).map_err(db_code)?;
            let (id, name) = u.ok_or(404u16)?;
            let (rows, more) = site.feed(Q::AuthorPosts, &[Val::Int(id)], page)?;
            if rows.is_empty() && page == 1 {
                // Only people who have published have a page.
                return Err(404);
            }
            pages::author_main(h, handle, &name, &rows, page, more);
            Ok(name)
        })
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
            // A draft (only its authors see it): as it is now.
            _ if !m.published => self.draft(m.id)?.0,
            Some((b, _)) => b,
            None => {
                let mut md = Vec::new();
                self.st.q(Q::PostBody, &[Val::Int(m.id as i64)], |row| md = row.bytes(0).to_vec()).map_err(db_code)?;
                let b = Rc::new(crate::markdown::render(&md));
                self.cache.put(m.id, m.updated_ms, b.clone(), 0);
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
            preview: false,
        };
        let s = r.signed_in();
        if !m.published {
            self.html(r, out, 200, |h| pages::post(h, s, &v, &body, None));
            return Ok(());
        }
        let (likes, count) = self.social_counts(m.id)?;
        let liked = r.signed_in() && self.st.one(Q::Liked, &[Val::Int(m.id as i64), Val::Int(r.uid() as i64)]).map_err(db_code)?.is_some();
        // (No comments, the common case: nothing more to ask. A deleted
        // comment still counts as there, for its replies.)
        let (cs, more_after, last) = if self.st.one(Q::AnyComment, &[Val::Int(m.id as i64)]).map_err(db_code)?.is_none() {
            (Vec::new(), None, 0)
        } else {
            let (cs, more_after) = self.threads(m.id, 0, r.uid(), can_edit)?;
            let last = self.st.one(Q::LastComment, &[Val::Int(m.id as i64)]).map_err(db_code)?.unwrap_or(0) as u64;
            (cs, more_after, last)
        };
        let sc = pages::Social { likes, liked, count, comments: &cs, more_after, last };
        self.html(r, out, 200, |h| pages::post(h, s, &v, &body, Some(&sc)));
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
    fn email_link(st: &mut Store, conf: &Conf, token: &[u8; 32], email: &str, name: &str, handle: &str, purpose: i64, now: u64, next: &str) -> Result<String, u16> {
        let t = hex(token);
        st.run(Q::TokenNew, &[Val::Blob(&sha256(token)), Val::Text(email.as_bytes()), Val::Text(name.as_bytes()), Val::Text(handle.as_bytes()), Val::Int(purpose), Val::Int((now + TOKEN_MS) as i64)]).map_err(db_code)?;
        if !conf.direct {
            let (subject, what) = if purpose == 1 { ("Finish signing up", "finish signing up") } else { ("Add a passkey", "add a passkey to your account") };
            let body = format!("Open this link to {what}:\n\n{}{}\n\nIt works once, for 30 minutes. If you did not ask for this, ignore this email: nothing happens without the link.\n", conf.origin, verify_url(&t, next));
            st.run(Q::Outbox, &[Val::Text(email.as_bytes()), Val::Text(subject.as_bytes()), Val::Text(body.as_bytes()), Val::Int(now as i64)]).map_err(db_code)?;
        }
        Ok(t)
    }

    fn signup(&mut self, r: &mut R, f: &Form, out: &mut Vec<u8>) -> Res {
        let now = r.now();
        let name = f.text("name", 80, false);
        let email = f.get("email").map(|e| e.trim().to_ascii_lowercase()).filter(|e| email_ok(e));
        let handle = f.get("handle").and_then(Site::handle_of);
        let next = safe_next(f.get("next"));
        let nx = next.as_deref().unwrap_or("");
        let (Some(name), Some(email), Some(handle)) = (name.clone(), email.clone(), handle.clone()) else {
            let (n, e, hd) = (f.get("name").unwrap_or(""), f.get("email").unwrap_or(""), f.get("handle").unwrap_or(""));
            let (n, e, hd) = (n.chars().take(80).collect::<String>(), e.chars().take(254).collect::<String>(), hd.chars().take(31).collect::<String>());
            // What exactly is wrong, field by field.
            let mut note = String::new();
            if name.is_none() {
                note.push_str("Your name: 1 to 80 characters, on one line. ");
            }
            if handle.is_none() {
                note.push_str("Username: 3 to 30 of a-z, 0-9 and _, starting with a letter. ");
            }
            if email.is_none() {
                note.push_str("Email: an address like you@example.com.");
            }
            self.html(r, out, 400, |h| pages::signup(h, note.trim(), &n, &hd, &e, nx));
            return Ok(());
        };
        if self.handle_taken(&handle, &email, now)? {
            self.html(r, out, 409, |h| pages::signup(h, "That username is taken. Choose another.", &name, "", &email, nx));
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
            Site::email_link(st, conf, &token, &email, &name, &handle, 1, now, nx).map(Some)
        })?;
        match (res, self.conf.direct) {
            (Some(t), true) => resp::redirect(out, &verify_url(&t, nx), b""),
            (None, true) => self.html(r, out, 409, |h| pages::signup(h, "This email may already have an account: log in instead.", &name, &handle, &email, nx)),
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
            Site::email_link(st, conf, &token, &email, &nh.0, &nh.1, 2, now, "").map(|_| ())
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
    /// Signed in, or sent to log in, and then back here (?next=).
    fn signed(&self, r: &R, out: &mut Vec<u8>) -> bool {
        if !r.signed_in() {
            let here = if r.query.is_empty() { r.path.to_string() } else { format!("{}?{}", r.path, r.query) };
            resp::redirect(out, &format!("/login?next={}", crate::form::encode(&here)), b"");
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
        let mut ids = vec![];
        self.st.q(Q::BlogPostIds, &[Val::Int(bid as i64)], |row| ids.push(row.int(0) as u64)).map_err(db_code)?;
        self.st.write(&p, r.now(), true, |st, _| Ok(st.run(Q::BlogDel, &[Val::Int(bid as i64)])?)).map_err(no_code)?;
        // (Memory only: ids are never reused, so these could not be served
        // for other posts anyway; see the schema.)
        for id in ids {
            self.docs.forget(id);
            if let Some((_, m, _)) = self.cache.map.remove(&id) {
                self.cache.bytes -= m.len();
            }
            if let Some((_, m, _)) = self.drafts.map.remove(&id) {
                self.drafts.bytes -= m.len();
            }
        }
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
        // A replica number for this page's editor: the ids it makes. Each
        // is a row, so page loads are limited (OPENS_PER_MINUTE a user).
        let uid = r.uid();
        let minute = r.now() / 60_000;
        let n = self.opens.entry(uid).or_insert((minute, 0));
        if n.0 != minute {
            *n = (minute, 0);
        }
        n.1 += 1;
        if n.1 > OPENS_PER_MINUTE {
            return Err(429);
        }
        if self.opens.len() > 100_000 {
            self.opens.retain(|_, v| v.0 == minute);
        }
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

    /// The post as it would publish: the document's text now, rendered, for
    /// its authors (THE POLICY: EditPost).
    fn preview(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        if !self.signed(r, out) {
            return Ok(());
        }
        let id = Site::post_id(id)?;
        self.permit(r, 0, id, Action::EditPost { post: id })?;
        let mut info = None;
        self.st
            .q(Q::PreviewInfo, &[Val::Int(id as i64)], |row| {
                info = Some((row.text(0).to_string(), row.text(1).to_string(), row.text(2).to_string(), row.text(3).to_string(), row.text(4).to_string(), row.int(5)))
            })
            .map_err(db_code)?;
        let (saved_title, blog_slug, blog_title, author, handle, published_ms) = info.ok_or(404u16)?;
        // The title as the editor has it now (not saved yet), if it sent one.
        let title = Form::parse(r.query.as_bytes()).and_then(|q| q.text("title", 200, false)).unwrap_or(saved_title);
        let (body, words) = self.draft(id)?;
        let v = pages::PostView {
            id,
            blog_slug: &blog_slug,
            blog_title: &blog_title,
            title: &title,
            author: &author,
            handle: &handle,
            published_ms: if published_ms > 0 { published_ms } else { r.now() as i64 },
            words,
            published: false,
            can_edit: true,
            just_published: false,
            preview: true,
        };
        let s = r.signed_in();
        self.html(r, out, 200, |h| pages::post(h, s, &v, &body, None));
        Ok(())
    }

    /// The editor's current text rendered, and its words: once per version
    /// of the document (a novel takes 70-80 ms to render; a draft's page
    /// and its preview were rendered again on every view).
    fn draft(&mut self, id: u64) -> Result<(Rc<Markup>, i64), u16> {
        // (Brought up to date with the database first: its seq is the version.)
        self.docs.get(&mut self.st, id).map_err(no_code)?;
        let seq = self.docs.seq(id);
        if let Some(hit) = self.drafts.get(id, seq) {
            return Ok(hit);
        }
        let md = self.docs.get(&mut self.st, id).map_err(no_code)?.text();
        let body = Rc::new(crate::markdown::render(md.as_bytes()));
        let words = crate::markdown::words(md.as_bytes()) as i64;
        self.drafts.put(id, seq, body.clone(), words);
        Ok((body, words))
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
        let dest = self.st.write(&p, now, true, |st, _| {
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
            });
        if dest.is_err() {
            // (As in sync: the copy may hold what was rolled back.)
            self.docs.forget(id);
        }
        let dest = dest.map_err(no_code)?;
        if body.is_some() {
            self.changes.bump(Kind::Doc, id);
        }
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
        self.cache.map.remove(&id).map(|(_, m, _)| self.cache.bytes -= m.len());
        self.drafts.map.remove(&id).map(|(_, m, _)| self.drafts.bytes -= m.len());
        self.docs.forget(id);
        resp::redirect(out, &format!("/dash/{slug}"), b"");
        Ok(())
    }

    // THE EDITOR'S SYNC (local-first: the browser holds the document and
    // sends its operations; the answer brings everyone else's). Body:
    // since (u64: the last stored batch the editor has), rep (u32: its
    // replica number), then operations (src/crdt.rs wire format).
    // ?me=<rep>: the asker page's own replica number, whose batches it
    // has and is not sent back (not the body's rep: a page may send edits
    // restored from an earlier page load, whose stored batches it lacks).
    // ?wait=1 with no operations: if nothing is new, the request waits
    // (parked: src/server.rs) until something is, or WAIT_MS: others'
    // changes are pushed, not polled for.
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
            let stored = self.st.write(&p, now, false, |st, _| {
                docs.store(st, id, rep, ops)?;
                st.run(Q::Edited, &[Val::Int(id as i64), Val::Int(now as i64)])?;
                Ok(())
            });
            if stored.is_err() {
                // The batch went into this worker's copy of the document,
                // and the transaction was rolled back: the copy is no
                // longer the database's. (Else the retry looks like a
                // repeat, is not stored, and later batches that build on
                // it are: found by the whole-app simulation.)
                self.docs.forget(id);
            }
            stored.map_err(no_code)?;
        }
        if !ops.is_empty() {
            self.changes.bump(Kind::Doc, id);
        }
        let q = Form::parse(r.query.as_bytes()).ok_or(400u16)?;
        let wait = ops.is_empty() && q.get("wait") == Some("1");
        // (Only ever leaves the asker's own data out: anything is safe here.)
        let mine = q.num("me").unwrap_or(0).min(u32::MAX as u64) as u32;
        // (Noted before the database is read: a change committed after the
        // read moves it, and wakes the wait.)
        let seen = self.changes.get(Kind::Doc, id);
        let mut body = Vec::new();
        self.docs.reply(&mut self.st, id, since, mine, &mut body).map_err(no_code)?;
        if wait && nothing_new(&body) && self.park(r, id, Waiting::Doc { since, rep: mine }, seen) {
            return Ok(());
        }
        resp::whole(out, 200, "application/octet-stream", Cache::NoStore, None, b"", &body, true);
        Ok(())
    }

    /// Parks this request until the post changes (seen: its counter
    /// before the database was read). False: too many wait already.
    fn park(&mut self, r: &mut R, post: u64, what: Waiting, seen: u32) -> bool {
        let uid = r.uid();
        // (Readers signed out count as one user: 0.)
        if self.waits.len() >= WAITS_MAX || self.waits.iter().filter(|w| w.uid == uid).count() >= if uid == 0 { WAITS_MAX / 2 } else { WAITS_PER_USER } {
            return false;
        }
        self.waits.push(Wait { conn: r.cx.conn, post, uid, what, seen, until: r.cx.mono_ms + self.conf.wait_ms });
        r.cx.park = true;
        true
    }

    /// Answers the waiting requests whose post changed, or whose time is
    /// up (now: monotonic ms). Whether the asker may still see the post
    /// (or edit it) is decided again: it may have changed meanwhile.
    fn answer_waits(&mut self, now: u64, answers: &mut Vec<(u64, Vec<u8>)>) {
        let mut i = 0;
        while i < self.waits.len() {
            let w = &self.waits[i];
            let cur = self.changes.get(w.what.kind(), w.post);
            if cur == w.seen && now < w.until {
                i += 1;
                continue;
            }
            let (conn, post, uid, late) = (w.conn, w.post, w.uid, now >= w.until);
            let mut out = Vec::new();
            let answered = match self.waits[i].what {
                Waiting::Doc { since, rep } => self.doc_answer(post, uid, since, rep, late, &mut out),
                Waiting::Comments { after, n } => self.comments_answer(post, uid, after, n, late, &mut out),
            };
            if !answered {
                // (Another post's change, the counter being shared, or the
                // asker's own.)
                self.waits[i].seen = cur;
                i += 1;
                continue;
            }
            answers.push((conn, out));
            self.waits.swap_remove(i);
        }
    }

    /// A parked sync's answer, if there is one to give (late: its time is up).
    fn doc_answer(&mut self, post: u64, uid: u64, since: i64, rep: u32, late: bool, out: &mut Vec<u8>) -> bool {
        let allowed = self.st.facts(uid, 0, post).ok().and_then(|f| authorize(f, Action::EditPost { post }));
        if allowed.is_none() {
            crate::server::error(out, 403);
            return true;
        }
        let mut body = Vec::new();
        if self.docs.reply(&mut self.st, post, since, rep, &mut body).is_err() {
            crate::server::error(out, 503);
            return true;
        }
        if nothing_new(&body) && !late {
            return false;
        }
        resp::whole(out, 200, "application/octet-stream", Cache::NoStore, None, b"", &body, true);
        true
    }

    // COMMENTS AND LIKES (Datastar: every form and link also works without
    // JavaScript, as a page load; with it, the answer is patches)
    fn social_counts(&mut self, pid: u64) -> Result<(i64, i64), u16> {
        let mut c = (0, 0);
        self.st.q(Q::Social, &[Val::Int(pid as i64)], |row| c = (row.int(0), row.int(1))).map_err(db_code)?;
        Ok(c)
    }

    fn comment_rows(&mut self, q: Q, args: &[Val], uid: u64, moderator: bool) -> Result<Vec<pages::CommentView>, u16> {
        let mut v = Vec::new();
        let comments_md = &mut self.comments_md;
        self.st
            .q(q, args, |row| {
                let author_id = row.int(7) as u64;
                v.push(pages::CommentView {
                    id: row.int(0) as u64,
                    parent: row.int(1) as u64,
                    author: row.text(2).into(),
                    handle: row.text(3).into(),
                    created_ms: row.int(5),
                    deleted: row.int(6) != 0,
                    can_delete: uid != 0 && (author_id == uid || moderator),
                    // (A comment's text never changes: its rendering is
                    // cached by id; a deleted one shows none.)
                    body: {
                        let id = row.int(0) as u64;
                        match comments_md.get(&id) {
                            Some(m) => Rc::clone(m),
                            None => {
                                let m = Rc::new(crate::markdown::render(row.bytes(4)));
                                if comments_md.len() >= COMMENT_CACHE {
                                    comments_md.clear();
                                }
                                comments_md.insert(id, Rc::clone(&m));
                                m
                            }
                        }
                    },
                })
            })
            .map_err(db_code)?;
        Ok(v)
    }

    /// THREADS_PAGE threads after thread id `after`, with their replies (at
    /// most REPLIES_MAX); and where the next page starts, if there is one.
    fn threads(&mut self, pid: u64, after: u64, uid: u64, moderator: bool) -> Result<(Vec<pages::CommentView>, Option<u64>), u16> {
        let mut top = self.comment_rows(Q::Threads, &[Val::Int(pid as i64), Val::Int(after as i64), Val::Int(THREADS_PAGE + 1)], uid, moderator)?;
        let more = top.len() > THREADS_PAGE as usize;
        top.truncate(THREADS_PAGE as usize);
        let (Some(a), Some(z)) = (top.first().map(|c| c.id), top.last().map(|c| c.id)) else { return Ok((top, None)) };
        let replies = self.comment_rows(Q::Replies, &[Val::Int(pid as i64), Val::Int(a as i64), Val::Int(z as i64), Val::Int(REPLIES_MAX)], uid, moderator)?;
        let mut all = top;
        all.extend(replies);
        all.sort_by_key(|c| c.id);
        Ok((all, if more { Some(z) } else { None }))
    }

    /// The facts for a published post's comments: its id from the path,
    /// may the viewer moderate.
    fn public_post(&mut self, r: &R, id: &str) -> Result<(u64, bool), u16> {
        let pid = Site::post_id(id)?;
        let p = self.permit(r, 0, pid, Action::ReadPost { post: pid }).map_err(|_| 404u16)?;
        let f = p.facts();
        if !f.post.as_ref().is_some_and(|x| x.published) {
            return Err(404);
        }
        Ok((pid, f.role.is_some()))
    }

    /// Comments after `after` as patches (each into its parent's replies, or
    /// the thread), the cursor moved, and extra patches first.
    fn comments_after(&mut self, uid: u64, signed_in: bool, pid: u64, after: u64, moderator: bool, extra: &[(String, &'static str, Vec<u8>)], out: &mut Vec<u8>) -> Res {
        let cs = self.comment_rows(Q::CommentsAfter, &[Val::Int(pid as i64), Val::Int(after as i64), Val::Int(200)], uid, moderator)?;
        let mut ps: Vec<(String, &'static str, Vec<u8>)> = extra.to_vec();
        let mut last = after;
        for c in &cs {
            let mut h = H::new(String::new());
            pages::comment_live(&mut h, c, signed_in);
            let target = if c.parent == 0 { "#thread".to_string() } else { format!("#r{}", c.parent) };
            ps.push((target, "append", h.b));
            last = c.id;
        }
        let refs: Vec<(&str, &str, &[u8])> = ps.iter().map(|(a, b, c)| (a.as_str(), *b, c.as_slice())).collect();
        // (_sending too: a sent comment's answer replaces the form whose
        // request it is, so Datastar never gets to reset its indicator.)
        let sig = format!("{{\"cafter\": {last}, \"_sending\": false}}");
        resp::patches_signals(out, &refs, Some(&sig));
        Ok(())
    }

    // LIVE COMMENTS: the request waits (parked) until someone comments, or
    // WAIT_MS; its answer brings the new comments and the next request.
    fn live(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let (pid, moderator) = self.public_post(r, id)?;
        let q = Form::parse(r.query.as_bytes()).ok_or(400u16)?;
        let after = q.num("after").unwrap_or(0);
        let n = q.num("n").unwrap_or(0);
        let seen = self.changes.get(Kind::Comments, pid);
        let none = self.st.one(Q::CommentsAfter, &[Val::Int(pid as i64), Val::Int(after as i64), Val::Int(1)]).map_err(db_code)?.is_none();
        if none && self.park(r, pid, Waiting::Comments { after, n }, seen) {
            return Ok(());
        }
        // (Not waiting here, when too many are: the next request after a pause.)
        let mut h = H::new(String::new());
        pages::live(&mut h, pid, n + 1, if none { LIVE_PAUSE } else { LIVE_NEXT });
        self.comments_after(r.uid(), r.signed_in(), pid, after, moderator, &[("#live".into(), "outer", h.b)], out)
    }

    /// A parked live request's answer, if there is one to give.
    fn comments_answer(&mut self, pid: u64, uid: u64, after: u64, n: u64, late: bool, out: &mut Vec<u8>) -> bool {
        let f = match self.st.facts(uid, 0, pid) {
            Ok(f) => f,
            Err(_) => {
                crate::server::error(out, 503);
                return true;
            }
        };
        let moderator = f.role.is_some();
        let published = f.post.as_ref().is_some_and(|x| x.published);
        if authorize(f, Action::ReadPost { post: pid }).is_none() || !published {
            crate::server::error(out, 404);
            return true;
        }
        let none = matches!(self.st.one(Q::CommentsAfter, &[Val::Int(pid as i64), Val::Int(after as i64), Val::Int(1)]), Ok(None));
        if none && !late {
            return false;
        }
        let mut h = H::new(String::new());
        pages::live(&mut h, pid, n + 1, LIVE_NEXT);
        if self.comments_after(uid, uid != 0, pid, after, moderator, &[("#live".into(), "outer", h.b)], out).is_err() {
            out.clear();
            crate::server::error(out, 503);
        }
        true
    }

    fn more(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let (pid, moderator) = self.public_post(r, id)?;
        let after = Form::parse(r.query.as_bytes()).and_then(|q| q.num("after")).unwrap_or(0);
        let (cs, more) = self.threads(pid, after, r.uid(), moderator)?;
        let mut h = H::new(String::new());
        h.r("<div class=\"cbatch\">");
        pages::thread(&mut h, &cs, r.signed_in());
        h.r("</div>");
        let mut m = H::new(String::new());
        if let Some(a) = more {
            pages::more_comments(&mut m, pid, a);
        }
        if r.ds {
            let mode = if more.is_some() { "outer" } else { "remove" };
            resp::patches(out, &[("#more-comments", "before", &h.b), ("#more-comments", mode, &m.b)]);
            return Ok(());
        }
        let s = r.signed_in();
        self.html(r, out, 200, |p| {
            pages::open(p, "Comments", s, false);
            p.r("<section class=\"comments\" id=\"comments\"><div id=\"thread\">");
            p.b.extend_from_slice(&h.b);
            p.b.extend_from_slice(&m.b);
            p.r("</div></section>");
            pages::close(p);
        });
        Ok(())
    }

    fn reply(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        if !self.signed(r, out) {
            return Ok(());
        }
        let cid = Site::post_id(id)?;
        let mut c = None;
        self.st.q(Q::CommentGet, &[Val::Int(cid as i64)], |row| c = Some(row.int(0) as u64)).map_err(db_code)?;
        let pid = c.ok_or(404u16)?;
        self.public_post(r, &pid.to_string())?;
        let mut h = H::new(String::new());
        pages::comment_box(&mut h, pid, cid, "rff", "Write a reply", r.ds);
        if r.ds {
            resp::patches(out, &[(&format!("#rf{cid}"), "inner", &h.b)]);
            return Ok(());
        }
        let s = r.signed_in();
        self.html(r, out, 200, |p| {
            pages::open(p, "Reply", s, false);
            p.r("<h1>Reply</h1>");
            p.b.extend_from_slice(&h.b);
            pages::close(p);
        });
        Ok(())
    }

    fn comment(&mut self, r: &mut R, id: &str, f: &Form, out: &mut Vec<u8>) -> Res {
        let pid = Site::post_id(id)?;
        let body = f.text("body", COMMENT_MAX, true).ok_or(400u16)?.replace("\r\n", "\n");
        let parent = f.num("parent").unwrap_or(0);
        let p = self.permit(r, 0, pid, Action::Comment { post: pid })?;
        let moderator = p.facts().role.is_some();
        let (uid, now) = (r.uid(), r.now());
        let cid = self
            .st
            .write(&p, now, true, |st, _| {
                let root = if parent == 0 {
                    None
                } else {
                    let mut got = None;
                    st.q(Q::CommentGet, &[Val::Int(parent as i64)], |row| got = Some((row.int(0) as u64, row.int(2))))?;
                    match got {
                        Some((post, root)) if post == pid => Some(root),
                        _ => return Err(No::Bad),
                    }
                };
                let par = if parent == 0 { Val::Null } else { Val::Int(parent as i64) };
                let rt = root.map_or(Val::Null, Val::Int);
                st.run(Q::CommentNew, &[Val::Int(pid as i64), par, rt, Val::Int(uid as i64), Val::Text(body.as_bytes()), Val::Int(now as i64)])?;
                let cid = st.db.last_rowid();
                st.run(Q::CommentCount, &[Val::Int(pid as i64), Val::Int(1)])?;
                Ok(cid)
            })
            .map_err(no_code)?;
        self.changes.bump(Kind::Comments, pid);
        if r.ds {
            // The box emptied (a reply's closed), then everything new.
            let mut h = H::new(String::new());
            let extra = if parent == 0 {
                pages::comment_box(&mut h, pid, 0, "cform", "Write a comment", false);
                vec![("#cform".to_string(), "replace", h.b)]
            } else {
                vec![(format!("#rf{parent}"), "inner", Vec::new())]
            };
            let after = f.num("after").unwrap_or(0);
            return self.comments_after(r.uid(), r.signed_in(), pid, after, moderator, &extra, out);
        }
        let back = self.post_url(pid)?;
        resp::redirect(out, &format!("{back}#c{cid}"), b"");
        Ok(())
    }

    fn post_url(&mut self, pid: u64) -> Result<String, u16> {
        let mut u = None;
        self.st.q(Q::EditGet, &[Val::Int(pid as i64)], |row| u = Some(format!("/b/{}/{}", row.text(3), row.text(1)))).map_err(db_code)?;
        u.ok_or(404)
    }

    fn delete_comment(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let cid = Site::post_id(id)?;
        let mut c = None;
        self.st.q(Q::CommentGet, &[Val::Int(cid as i64)], |row| c = Some((row.int(0) as u64, row.int(1) as u64))).map_err(db_code)?;
        let (pid, author) = c.ok_or(404u16)?;
        let p = self.permit(r, 0, pid, Action::DeleteComment { post: pid, author })?;
        self.st
            .write(&p, r.now(), true, |st, _| {
                // The author the permit was decided on, still.
                let mut now_author = None;
                st.q(Q::CommentGet, &[Val::Int(cid as i64)], |row| now_author = Some(row.int(1) as u64))?;
                if now_author != Some(author) {
                    return Err(No::Denied);
                }
                if st.run(Q::CommentDel, &[Val::Int(cid as i64)])? == 1 {
                    st.run(Q::CommentCount, &[Val::Int(pid as i64), Val::Int(-1)])?;
                }
                Ok(())
            })
            .map_err(no_code)?;
        if r.ds {
            resp::patches(out, &[(&format!("#cb{cid}"), "inner", b"<p class=\"muted\">[deleted]</p>"), (&format!("#ca{cid} details.del"), "remove", b"")]);
            return Ok(());
        }
        let back = self.post_url(pid)?;
        resp::redirect(out, &format!("{back}#c{cid}"), b"");
        Ok(())
    }

    fn like(&mut self, r: &mut R, id: &str, f: &Form, out: &mut Vec<u8>) -> Res {
        let pid = Site::post_id(id)?;
        let on = f.get("on") != Some("0");
        let p = self.permit(r, 0, pid, Action::Like { post: pid })?;
        let (uid, now) = (r.uid(), r.now());
        self.st
            .write(&p, now, true, |st, _| {
                let changed = if on {
                    st.run(Q::LikeAdd, &[Val::Int(pid as i64), Val::Int(uid as i64), Val::Int(now as i64)])?
                } else {
                    st.run(Q::LikeDel, &[Val::Int(pid as i64), Val::Int(uid as i64)])?
                };
                if changed == 1 {
                    st.run(Q::LikeCount, &[Val::Int(pid as i64), Val::Int(if on { 1 } else { -1 })])?;
                }
                Ok(())
            })
            .map_err(no_code)?;
        if r.ds {
            let (likes, count) = self.social_counts(pid)?;
            let liked = self.st.one(Q::Liked, &[Val::Int(pid as i64), Val::Int(uid as i64)]).map_err(db_code)?.is_some();
            let mut h = H::new(String::new());
            pages::social(&mut h, true, pid, likes, liked, count);
            resp::patches(out, &[("", "", &h.b)]);
            return Ok(());
        }
        let back = self.post_url(pid)?;
        resp::redirect(out, &back, b"");
        Ok(())
    }

    // IMAGES: sent by the editor (resized there), kept by an unguessable
    // key, served as what their first bytes say they are.
    fn upload(&mut self, r: &mut R, id: &str, out: &mut Vec<u8>) -> Res {
        let pid = Site::post_id(id)?;
        let b = r.req.body;
        let ctype = image_type(b).ok_or(415u16)?;
        if b.len() > IMAGE_MAX {
            return Err(413);
        }
        let p = self.permit(r, 0, pid, Action::EditPost { post: pid })?;
        let key = hex(&r.random::<16>());
        let (uid, now) = (r.uid(), r.now());
        self.st
            .write(&p, now, true, |st, _| Ok(st.run(Q::ImageNew, &[Val::Text(key.as_bytes()), Val::Int(pid as i64), Val::Int(uid as i64), Val::Text(ctype.as_bytes()), Val::Blob(b), Val::Int(now as i64)])?))
            .map_err(no_code)?;
        Site::text(out, 200, format!("/img/{key}").as_bytes(), b"");
        Ok(())
    }

    fn image(&mut self, r: &mut R, key: &str, out: &mut Vec<u8>) -> Res {
        if key.len() != 32 || !key.bytes().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()) {
            return Err(404);
        }
        let mut img = None;
        self.st.q(Q::ImageGet, &[Val::Text(key.as_bytes())], |row| img = Some((row.text(0).to_string(), row.bytes(1).to_vec()))).map_err(db_code)?;
        let (ctype, bytes) = img.ok_or(404u16)?;
        // (The type is re-derived from the bytes: what is served matches them.)
        let t = image_type(&bytes).ok_or(404u16)?;
        debug_assert_eq!(t, ctype);
        resp::whole(out, 200, t, Cache::Immutable, None, b"", &bytes, !r.head_only);
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
            st.commit().map_err(db_code)?;
            Ok(v)
        }
        Err(e) => {
            let _ = st.run(Q::Rollback, &[]);
            Err(e)
        }
    }
}

/// Where to go after logging in or signing up: a path on this site (not
/// another site's address, not a log-in page again), or nothing.
fn safe_next(n: Option<&str>) -> Option<String> {
    let n = n?;
    let ok = n.starts_with('/')
        && !n.starts_with("//")
        && n.len() <= 1024
        && n.bytes().all(|c| c.is_ascii_graphic() && c != b'\\')
        && !["/login", "/signup", "/verify", "/recover"].iter().any(|p| n == *p || n.starts_with(&format!("{p}?")) || n.starts_with(&format!("{p}/")));
    if ok { Some(n.to_string()) } else { None }
}

/// The emailed link (and, in test mode, the page) for a token, with where
/// to go afterwards.
fn verify_url(t: &str, next: &str) -> String {
    if next.is_empty() { format!("/verify?t={t}") } else { format!("/verify?t={t}&next={}", crate::form::encode(next)) }
}

/// An image's type from its first bytes (JPEG, PNG, GIF, WebP), or None.
fn image_type(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
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

/// A sync reply with nothing in it: no snapshot, no batches (from others),
/// no more to come.
fn nothing_new(body: &[u8]) -> bool {
    body.len() == 10 && body[0] == 0 && body[9] == 0
}

/// What the site holds in memory, and each part's budget (the
/// simulator's invariants).
pub struct Sizes {
    pub parts: Vec<(&'static str, usize, usize)>,
}

impl Site {
    pub fn sizes(&self) -> Sizes {
        let (docs, docs_max) = self.docs.memory();
        Sizes {
            parts: vec![
                ("documents", docs, docs_max),
                ("post pages", self.cache.bytes, self.cache.budget),
                ("drafts", self.drafts.bytes, self.drafts.budget),
                ("feed pages", self.feeds.bytes, FEEDS_BYTES),
                ("feed entries", self.feeds.map.len(), FEEDS_MAX),
                ("rendered comments", self.comments_md.len(), COMMENT_CACHE),
                ("waiting requests", self.waits.len(), WAITS_MAX),
            ],
        }
    }

    /// The store, for the simulator (to inject faults into).
    pub fn store(&mut self) -> &mut Store {
        &mut self.st
    }
}

impl App for Site {
    fn ready(&mut self, now_ms: u64, answers: &mut Vec<(u64, Vec<u8>)>) {
        if !self.waits.is_empty() {
            self.answer_waits(now_ms, answers);
        }
    }

    fn handle(&mut self, req: &Request, cx: &mut Ctx, out: &mut Vec<u8>) -> bool {
        let now = cx.now_ms;
        self.sweep(now);
        if self.stmt_stats {
            self.requests += 1;
            if self.requests % 5000 == 0 {
                eprintln!("reprepares after {} requests: {:?}", self.requests, self.st.db.reprepares());
            }
        }
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
