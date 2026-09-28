// THE ORACLE of the whole-application simulation: it sees every request
// and its answer at the moment the answer is made (a wrapper around each
// worker's Site), and decides whether the answer is allowed from facts it
// reads from the database itself, with its own SQL (never the site's code):
//   - every piece of a post's text in an answer (the simulated writers type
//     tokens "tk<post>x<n>q", each naming the post it was written in) is
//     from the post the request is about, of a post that still exists, and
//     readable by the asker: a member of its blog, or anyone if it is in
//     the published text (the law: spec/authz.rs ReadPost);
//   - every comment in an answer ("cm<post>x<n>q") exists, is not deleted,
//     and its post is readable by the asker;
//   - every change a request makes to content is one the law permits,
//     decided by law.rs (proved equal to spec/authz.rs `permitted`, and
//     apart from the server's src/authz.rs) on facts read here before the
//     request;
//   - every rendered post or comment body is allowed markup (an
//     independent checker of spec/markup.rs's law: tested, not proved);
//   - no page shows someone signed in whose session has ended by the wall
//     clock;
//   - THE OTHER SIDE: what the law permits succeeds. A request the law
//     allows (on the facts read here), that no injected fault touched,
//     is answered with success; the only refusals allowed are the limits
//     (the write budget, editor page loads a minute: 429) and a taken
//     address (409). A server that refuses everything fails here.
use super::Shared;
use super::law::permits;
use crate::server::{App, Ctx, Request};
use crate::site::Site;
use crate::spec_authz::{Action, Facts, PostInfo, Role};
use crate::sys::crypto::sha256;
use crate::sys::sqlite::{Db, Val};
use std::cell::RefCell;
use std::rc::Rc;

// THE PROBE: the oracle's own connection and statements.
pub struct Probe {
    db: Db,
    ids: Vec<usize>,
}

#[derive(Clone, Copy)]
pub enum P {
    Who,
    Role,
    Post,
    PostBySlug,
    BlogBySlug,
    InBody,
    CommentLive,
    CommentOf,
    UserByEmail,
    Fingerprint,
    MinDate,
    Snap,
    Ops,
    MyBlogs,
    BlogPosts,
    Published,
    Members,
    MyComments,
    PostSlug,
    Budget,
    PostComments,
    Mail,
    Traced,
    BadCounts,
    MailFlood,
    ImageExists,
    RepOwner,
    FeedAll,
    FeedBlog,
    FeedAuthor,
    AllPosts,
    AllComments,
    AllBlogs,
}

const SQL: &[&str] = &[
    "SELECT user_id FROM session WHERE token_hash = ?1 AND expires_ms > ?2",
    "SELECT role FROM member WHERE blog_id = ?1 AND user_id = ?2",
    "SELECT blog_id, published, title, draft_title FROM post WHERE id = ?1",
    "SELECT p.id FROM post p JOIN blog b ON b.id = p.blog_id WHERE b.slug = ?1 AND p.slug = ?2",
    "SELECT id FROM blog WHERE slug = ?1",
    "SELECT instr(body_md, ?2) > 0 FROM post WHERE id = ?1",
    "SELECT count(*) FROM comment WHERE post_id = ?1 AND deleted = 0 AND instr(body_md, ?2) > 0",
    "SELECT post_id, coalesce(author_id, 0) FROM comment WHERE id = ?1",
    "SELECT id FROM user WHERE email = ?1",
    "SELECT (SELECT count(*) || ':' || coalesce(sum(id), 0) FROM blog) || '|' || \
       (SELECT count(*) || ':' || coalesce(sum(blog_id * 131 + user_id * 7 + role), 0) FROM member) || '|' || \
       (SELECT count(*) || ':' || coalesce(sum(id + published * 3 + updated_ms % 1000003 + length(body_md)), 0) FROM post) || '|' || \
       (SELECT coalesce(max(seq), 0) FROM doc_ops) || '|' || \
       (SELECT count(*) || ':' || coalesce(sum(deleted), 0) FROM comment) || '|' || \
       (SELECT count(*) FROM post_like) || '|' || (SELECT count(*) FROM image)",
    "SELECT min(x) FROM (SELECT min(created_ms) AS x FROM user UNION ALL SELECT min(created_ms) FROM blog \
       UNION ALL SELECT min(updated_ms) FROM post UNION ALL SELECT min(created_ms) FROM comment \
       UNION ALL SELECT min(created_ms) FROM session UNION ALL SELECT min(expires_ms) FROM session)",
    "SELECT upto, data FROM doc_snap WHERE post_id = ?1",
    "SELECT data FROM doc_ops WHERE post_id = ?1 AND seq > ?2 ORDER BY seq",
    "SELECT b.id, b.slug, m.role FROM member m JOIN blog b ON b.id = m.blog_id WHERE m.user_id = ?1 ORDER BY b.id",
    "SELECT id FROM post WHERE blog_id = ?1 ORDER BY id",
    "SELECT p.id, b.slug, p.slug FROM post p JOIN blog b ON b.id = p.blog_id WHERE p.published = 1 ORDER BY p.id",
    "SELECT user_id, role FROM member WHERE blog_id = ?1 ORDER BY user_id",
    "SELECT id FROM comment WHERE author_id = ?1 AND deleted = 0 ORDER BY id",
    "SELECT b.slug, p.slug FROM post p JOIN blog b ON b.id = p.blog_id WHERE p.id = ?1",
    "SELECT n FROM write_budget WHERE user_id = ?1 AND window_ms > ?2 - 60000",
    "SELECT id FROM comment WHERE post_id = ?1 AND deleted = 0 ORDER BY id",
    "SELECT body FROM outbox WHERE to_email = ?1 ORDER BY id DESC LIMIT 1",
    "SELECT count(*) FROM doc_ops WHERE instr(data, cast(?1 AS blob)) > 0",
    "SELECT count(*) FROM post p WHERE p.like_count != (SELECT count(*) FROM post_like l WHERE l.post_id = p.id) \
       OR p.comment_count != (SELECT count(*) FROM comment c WHERE c.post_id = p.id AND c.deleted = 0)",
    "SELECT count(*) FROM outbox a WHERE (SELECT count(*) FROM outbox b WHERE b.to_email = a.to_email AND b.created_ms > a.created_ms - 3600000 AND b.created_ms <= a.created_ms) > 3",
    "SELECT count(*) FROM image WHERE key = ?1",
    "SELECT user_id FROM doc_rep WHERE post_id = ?1 AND rep = ?2",
    "SELECT id, published_ms FROM post WHERE published = 1 ORDER BY published_ms DESC",
    "SELECT p.id, p.published_ms FROM post p JOIN blog b ON b.id = p.blog_id WHERE b.slug = ?1 AND p.published = 1 ORDER BY p.published_ms DESC",
    "SELECT p.id, p.published_ms FROM post p JOIN user u ON u.id = p.author_id WHERE u.handle = ?1 AND p.published = 1 ORDER BY p.published_ms DESC",
    "SELECT p.id, b.slug, p.slug FROM post p JOIN blog b ON b.id = p.blog_id ORDER BY p.id",
    "SELECT id FROM comment ORDER BY id",
    "SELECT id, slug FROM blog ORDER BY id",
];

impl Probe {
    pub fn open(path: &str) -> Probe {
        let mut db = Db::open(path, false).expect("the probe opens the database");
        let ids = SQL.iter().map(|s| db.prepare(s).expect("probe statement")).collect();
        Probe { db, ids }
    }

    pub fn q(&mut self, p: P, args: &[Val], f: impl FnMut(&crate::sys::sqlite::Row)) {
        self.db.query(self.ids[p as usize], args, f).expect("probe query");
    }

    pub fn int(&mut self, p: P, args: &[Val]) -> Option<i64> {
        self.db.one_int(self.ids[p as usize], args).expect("probe query")
    }

    pub fn text(&mut self, p: P, args: &[Val]) -> String {
        let mut s = String::new();
        self.q(p, args, |r| s = r.text(0).to_string());
        s
    }

    /// The signed-in user for a session token, by the wall clock (0: none).
    pub fn who(&mut self, sid: Option<&[u8]>, wall: u64) -> u64 {
        let Some(t) = sid.and_then(unhex32) else { return 0 };
        self.int(P::Who, &[Val::Blob(&sha256(&t)), Val::Int(wall as i64)]).unwrap_or(0) as u64
    }

    /// (blog, published, identity) of a post.
    pub fn post(&mut self, id: u64) -> Option<(u64, bool, String)> {
        let mut r = None;
        self.q(P::Post, &[Val::Int(id as i64)], |row| r = Some((row.int(0) as u64, row.int(1) != 0, format!("{}|{}", row.text(2), row.text(3)))));
        r
    }

    pub fn role(&mut self, blog: u64, who: u64) -> Option<Role> {
        if who == 0 || blog == 0 {
            return None;
        }
        match self.int(P::Role, &[Val::Int(blog as i64), Val::Int(who as i64)]) {
            Some(1) => Some(Role::Owner),
            Some(2) => Some(Role::Author),
            _ => None,
        }
    }

    /// The facts a request is decided on, read here (as spec/authz.rs
    /// describes them: the blog is the post's when a post is named).
    pub fn facts(&mut self, who: u64, blog: u64, post: u64) -> Facts {
        let mut blog = blog;
        let mut info = None;
        if post != 0 {
            if let Some((b, published, _)) = self.post(post) {
                if blog == 0 {
                    blog = b;
                }
                info = Some(PostInfo { id: post, blog: b, published });
            }
        }
        let role = self.role(blog, who);
        Facts { who, blog, role, post: info }
    }

    pub fn member(&mut self, who: u64, post: u64) -> bool {
        match self.post(post) {
            Some((b, _, _)) => self.role(b, who).is_some(),
            None => false,
        }
    }

    /// The server's text of a post, from its snapshot and stored batches,
    /// applied by the CRDT here (not the site's cache).
    pub fn doc_text(&mut self, post: u64) -> Result<String, String> {
        self.doc(post).map(|d| d.text())
    }

    /// The post's document as stored.
    pub fn doc(&mut self, post: u64) -> Result<crate::crdt::Doc, String> {
        let mut snap = None;
        self.q(P::Snap, &[Val::Int(post as i64)], |r| snap = Some((r.int(0), r.bytes(1).to_vec())));
        let (upto, mut doc) = match snap {
            Some((u, d)) => (u, crate::crdt::Doc::load(&d).map_err(|e| format!("snapshot: {e:?}"))?),
            None => (0, crate::crdt::Doc::new()),
        };
        let mut batches = vec![];
        self.q(P::Ops, &[Val::Int(post as i64), Val::Int(upto)], |r| batches.push(r.bytes(0).to_vec()));
        for b in batches {
            doc.apply_batch(&b, &mut ()).map_err(|e| format!("batch: {e:?}"))?;
        }
        Ok(doc)
    }
}

pub fn unhex32(h: &[u8]) -> Option<[u8; 32]> {
    if h.len() != 64 {
        return None;
    }
    let v = |c: u8| (c as char).to_digit(16);
    let mut t = [0u8; 32];
    for i in 0..32 {
        t[i] = (v(h[2 * i])? * 16 + v(h[2 * i + 1])?) as u8;
    }
    Some(t)
}

// TOKENS: "tk<p>x<n>q" (a writer's text in post p) and "cm<p>x<n>q" (a
// comment on post p).
pub fn tokens(b: &[u8]) -> Vec<(u8, usize, String)> {
    let mut out = vec![];
    let mut i = 0;
    while i + 5 < b.len() {
        let kind = &b[i..i + 2];
        if kind == b"tk" || kind == b"cm" {
            let digits = |from: usize| b[from..].iter().take_while(|c| c.is_ascii_digit()).count();
            let a = digits(i + 2);
            if a > 0 && b.get(i + 2 + a) == Some(&b'x') {
                let n = digits(i + 3 + a);
                if n > 0 && b.get(i + 3 + a + n) == Some(&b'q') {
                    let p: usize = std::str::from_utf8(&b[i + 2..i + 2 + a]).expect("digits").parse().unwrap_or(usize::MAX);
                    let end = i + 4 + a + n;
                    out.push((kind[0], p, String::from_utf8_lossy(&b[i..end]).to_string()));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

// THE MARKUP LAW, checked (spec/markup.rs, read as a parser: every '<'
// starts an allowed tag, a link, an image, a code block's language or a
// list's start; text has no < > ").
pub fn markup_ok(h: &[u8]) -> Result<(), String> {
    const TAGS: &[&[u8]] = &[
        b"<p>", b"</p>", b"<h1>", b"</h1>", b"<h2>", b"</h2>", b"<h3>", b"</h3>", b"<h4>", b"</h4>", b"<h5>", b"</h5>", b"<h6>", b"</h6>",
        b"<strong>", b"</strong>", b"<em>", b"</em>", b"<code>", b"</code>", b"<pre>", b"</pre>", b"<blockquote>", b"</blockquote>",
        b"<ul>", b"</ul>", b"<ol>", b"</ol>", b"<li>", b"</li>", b"<br />", b"<hr />", b"</a>",
    ];
    let bad = |i: usize, why: &str| Err(format!("{why} at byte {i}: {:?}", String::from_utf8_lossy(&h[i.saturating_sub(20)..(i + 40).min(h.len())])));
    // A quoted value up to the next '"': (value, index of the '"').
    let value = |i: usize| -> Option<(&[u8], usize)> {
        let j = h[i..].iter().position(|&c| c == b'"')? + i;
        Some((&h[i..j], j))
    };
    let text_ok = |t: &[u8]| !t.iter().any(|&c| c == b'<' || c == b'>' || c == b'"');
    let mut i = 0;
    while i < h.len() {
        let c = h[i];
        if c != b'<' {
            if c == b'>' || c == b'"' {
                return bad(i, "a > or \" in text");
            }
            i += 1;
            continue;
        }
        if let Some(t) = TAGS.iter().find(|t| h[i..].starts_with(t)) {
            i += t.len();
            continue;
        }
        let rest = &h[i..];
        if rest.starts_with(b"<a href=\"") || rest.starts_with(b"<img src=\"") {
            let img = rest.starts_with(b"<img");
            let Some((u, j)) = value(i + if img { 10 } else { 9 }) else { return bad(i, "an unclosed URL") };
            if let Err(e) = url_ok(u) {
                return bad(i, &e);
            }
            i = j;
            if img {
                if !h[i..].starts_with(b"\" alt=\"") {
                    return bad(i, "an image without alt");
                }
                let Some((alt, j)) = value(i + 7) else { return bad(i, "an unclosed alt") };
                if !text_ok(alt) {
                    return bad(i, "alt text");
                }
                i = j;
            }
            if h[i..].starts_with(b"\" title=\"") {
                let Some((t, j)) = value(i + 9) else { return bad(i, "an unclosed title") };
                if !text_ok(t) {
                    return bad(i, "a title");
                }
                i = j;
            }
            let end: &[u8] = if img { b"\" />" } else { b"\">" };
            if !h[i..].starts_with(end) {
                return bad(i, "a link or image not ended as the law writes it");
            }
            i += end.len();
            continue;
        }
        if rest.starts_with(b"<code class=\"language-") || rest.starts_with(b"<ol start=\"") {
            let ol = rest.starts_with(b"<ol");
            let Some((v, j)) = value(i + if ol { 11 } else { 22 }) else { return bad(i, "an unclosed attribute") };
            let ok = if ol { !v.is_empty() && v.iter().all(|c| c.is_ascii_digit()) } else { text_ok(v) };
            if !ok || !h[j..].starts_with(b"\">") {
                return bad(i, "a list start or code language");
            }
            i = j + 2;
            continue;
        }
        return bad(i, "a tag not in the law's list");
    }
    Ok(())
}

/// A URL as written in an attribute: every & as &amp;, only visible ASCII
/// without < > " \ `, and no scheme but http, https, mailto.
fn url_ok(attr: &[u8]) -> Result<(), String> {
    let mut u = vec![];
    let mut i = 0;
    while i < attr.len() {
        if attr[i] == b'&' {
            if !attr[i..].starts_with(b"&amp;") {
                return Err("a bare & in a URL".into());
            }
            u.push(b'&');
            i += 5;
            continue;
        }
        u.push(attr[i]);
        i += 1;
    }
    if !u.iter().all(|&b| (0x21..=0x7e).contains(&b) && !b"<>\"\\`".contains(&b)) {
        return Err("a byte not allowed in a URL".into());
    }
    if u.first().is_some_and(|c| c.is_ascii_alphabetic()) {
        let k = u.iter().position(|&c| !(c.is_ascii_alphanumeric() || c == b'+' || c == b'-' || c == b'.')).unwrap_or(u.len());
        if k < u.len() && u[k] == b':' {
            let s = String::from_utf8_lossy(&u[..k]).to_ascii_lowercase();
            if s != "http" && s != "https" && s != "mailto" {
                return Err(format!("the scheme {s:?}"));
            }
        }
    }
    Ok(())
}

/// The bodies (posts' and comments') in a page.
fn bodies(page: &[u8]) -> Vec<&[u8]> {
    let mut out = vec![];
    for open in [&b"<div class=\"body\">"[..], &b"<div class=\"cbody\" id=\"cb"[..]] {
        let mut from = 0;
        while let Some(i) = find(&page[from..], open) {
            let mut s = from + i + open.len();
            if open.ends_with(b"cb") {
                // (past the id: `123">`)
                let Some(q) = find(&page[s..], b"\">") else { break };
                s += q + 2;
            }
            let Some(e) = find(&page[s..], b"</div>") else { break };
            out.push(&page[s..s + e]);
            from = s + e;
        }
    }
    out
}

pub fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

// THE WRAPPER.

/// What a request was about, as the oracle read it.
#[derive(Clone)]
struct Asked {
    user: usize,
    who: u64,
    what: String,
    /// The post the request is about (0: none).
    post: u64,
    dash: bool,
    /// A feed page: which (all, a blog's, an author's), its key, the page.
    feed: Option<(P, String, u64)>,
    /// A form that replaces a post's text (the editor without
    /// JavaScript): the post, the text, and the tokens in the text before.
    replace: Option<(u64, String, Vec<String>)>,
    /// An image asked for, by its key.
    image: Option<String>,
    /// When it was parked (monotonic), for a wait: push, not polling; and
    /// the waiting page's replica (its own changes do not wake it).
    parked_at: u64,
    doc_wait: bool,
    me: u32,
}

/// A batch well-formed by the wire format's own rules (src/crdt.rs's
/// comment, read here, not its checking code): runs of at least one
/// character with ids that fit in u32, deletes of at least one, replicas
/// and counters not 0, sides 0 or 1.
fn well_formed(ops: &[u8]) -> bool {
    let u = |i: usize| ops.get(i..i + 4).map(|b| u32::from_le_bytes(b.try_into().expect("4")) as u64);
    let mut i = 0;
    while i < ops.len() {
        match ops[i] {
            1 => {
                let (Some(rep), Some(ctr), Some(_pr), Some(_pc), Some(&side), Some(n)) = (u(i + 1), u(i + 5), u(i + 9), u(i + 13), ops.get(i + 17), u(i + 18)) else { return false };
                let Some(text) = ops.get(i + 22..i + 22 + n as usize).and_then(|t| std::str::from_utf8(t).ok()) else { return false };
                let chars = text.chars().count() as u64;
                if rep == 0 || ctr == 0 || side > 1 || chars == 0 || ctr + chars - 1 > u32::MAX as u64 {
                    return false;
                }
                i += 22 + n as usize;
            }
            2 => {
                let (Some(rep), Some(ctr), Some(len)) = (u(i + 1), u(i + 5), u(i + 9)) else { return false };
                if rep == 0 || ctr == 0 || len == 0 || ctr + len - 1 > u32::MAX as u64 {
                    return false;
                }
                i += 13;
            }
            _ => return false,
        }
    }
    true
}

/// An image, by its first bytes: PNG, JPEG, GIF, WebP (read here, not by
/// the server's check).
fn is_image(b: &[u8]) -> bool {
    b.starts_with(b"\x89PNG\r\n\x1a\n") || b.starts_with(b"\xff\xd8\xff") || b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") || (b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP")
}

/// A post's own address: 1 to 80 of a-z 0-9 -, not "draft-" (the schema's
/// and the form's rule, read here).
fn slug_ok(s: &str) -> bool {
    !s.is_empty() && s.len() <= 80 && !s.starts_with("draft-") && s.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// A feed page must show exactly the published posts of its place in the
/// order (newest first, PAGE_ITEMS a page); posts published in the same
/// millisecond at the page's edges may fall on either side.
fn feed_ok(sh: &mut Shared, which: P, key: &str, page: u64, body: &[u8]) -> Result<(), String> {
    const PAGE_ITEMS: usize = 20;
    let mut all: Vec<(u64, i64)> = vec![];
    let args: Vec<Val> = if key.is_empty() { vec![] } else { vec![Val::Text(key.as_bytes())] };
    sh.probe.q(which, &args, |r| all.push((r.int(0) as u64, r.int(1))));
    // The posts shown: by their links (href="/b/<blog>/<post>").
    let text = String::from_utf8_lossy(body);
    let mut shown: Vec<u64> = vec![];
    for part in text.split("href=\"/b/").skip(1) {
        let link = &part[..part.find('"').unwrap_or(0)];
        let Some((b, s)) = link.split_once('/') else { continue };
        let s = s.split('#').next().unwrap_or(s);
        match sh.probe.int(P::PostBySlug, &[Val::Text(b.as_bytes()), Val::Text(s.as_bytes())]) {
            Some(pid) => {
                if !shown.contains(&(pid as u64)) {
                    shown.push(pid as u64);
                }
            }
            // (A post page's own links, to its blog, are not posts.)
            None if !s.is_empty() => return Err(format!("page {page} links to a post that is not there: /b/{b}/{s}")),
            None => {}
        }
    }
    let from = (page as usize - 1) * PAGE_ITEMS;
    let want = all.len().saturating_sub(from).min(PAGE_ITEMS);
    if shown.len() != want {
        return Err(format!("page {page} shows {} posts, not {want} (of {} published)", shown.len(), all.len()));
    }
    if want == 0 {
        return Ok(());
    }
    let (hi, lo) = (all[from].1, all[from + want - 1].1);
    for &pid in &shown {
        match all.iter().find(|(p, _)| *p == pid) {
            None => return Err(format!("page {page} shows post {pid}, which is not published here")),
            Some(&(_, t)) if t > hi || t < lo => return Err(format!("page {page} shows post {pid}, published outside this page's place")),
            _ => {}
        }
    }
    for &(pid, t) in &all {
        if t < hi && t > lo && !shown.contains(&pid) {
            return Err(format!("page {page} leaves out post {pid}, which belongs on it"));
        }
    }
    Ok(())
}

pub struct Checked {
    pub site: Site,
    sh: Rc<RefCell<Shared>>,
    parked: crate::hash::Map<u64, Asked>,
    injected: Rc<std::cell::Cell<u64>>,
}

impl Checked {
    pub fn new(site: Site, sh: Rc<RefCell<Shared>>, injected: Rc<std::cell::Cell<u64>>) -> Checked {
        Checked { site, sh, parked: crate::hash::map(), injected }
    }
}

/// Whether a sync body's operations decode (a malformed batch is refused
/// whole: 400 is right for it).
fn decodes(ops: &[u8]) -> bool {
    let mut i = 0;
    while i < ops.len() {
        match crate::crdt::decode(ops, i) {
            Ok((_, n)) => i = n,
            Err(_) => return false,
        }
    }
    true
}

/// What a request the law permits must get: (the action, its facts, the
/// refusals still allowed).
fn expected(pr: &mut Probe, who: u64, method: &[u8], parts: &[&str], form: &crate::form::Form, body: &[u8]) -> Option<(Action, Facts, &'static [u16])> {
    if method == b"POST" {
        let (a, f) = action(pr, who, method, parts, form, body.len())?;
        let ok: &'static [u16] = match parts {
            ["blogs"] => &[409, 429],
            // (Syncing is not budgeted; a batch over the log's limit is 409;
            // one that is not operations at all, 400.)
            ["edit", _, "sync"] if !decodes(body.get(12..).unwrap_or(b"")) => &[400, 409],
            ["edit", _, "sync"] => &[409],
            _ => &[429],
        };
        return Some((a, f, ok));
    }
    let id = |s: &str| num(s);
    Some(match parts {
        ["b", b, s] => {
            let p = pr.int(P::PostBySlug, &[Val::Text(b.as_bytes()), Val::Text(s.as_bytes())]).unwrap_or(0) as u64;
            if p == 0 {
                return None;
            }
            (Action::ReadPost { post: p }, pr.facts(who, 0, p), &[])
        }
        ["edit", i] => (Action::EditPost { post: id(i) }, pr.facts(who, 0, id(i)), &[429]),
        ["edit", i, "preview"] => (Action::EditPost { post: id(i) }, pr.facts(who, 0, id(i)), &[]),
        // (Comments are a published post's: a draft has none to show.)
        ["live", i] | ["comments", i] => {
            let f = pr.facts(who, 0, id(i));
            if !f.post.as_ref().is_some_and(|p| p.published) {
                return None;
            }
            (Action::ReadPost { post: id(i) }, f, &[])
        }
        ["dash"] if who != 0 => (Action::CreateBlog, pr.facts(who, 0, 0), &[]),
        // (The reply page: signed in, a comment of a published post.)
        ["reply", c] if who != 0 => {
            let mut post = 0;
            pr.q(P::CommentOf, &[Val::Int(id(c) as i64)], |r| post = r.int(0) as u64);
            let f = pr.facts(who, 0, post);
            if post == 0 || !f.post.as_ref().is_some_and(|p| p.published) {
                return None;
            }
            (Action::ReadPost { post }, f, &[])
        }
        _ => return None,
    })
}

fn num(s: &str) -> u64 {
    s.parse().unwrap_or(0)
}

/// The action a request would take, and the facts it is decided on.
fn action(pr: &mut Probe, who: u64, method: &[u8], parts: &[&str], form: &crate::form::Form, body_len: usize) -> Option<(Action, Facts)> {
    if method != b"POST" {
        return None;
    }
    let blog = |pr: &mut Probe, slug: &str| pr.int(P::BlogBySlug, &[Val::Text(slug.as_bytes())]).unwrap_or(0) as u64;
    Some(match parts {
        ["blogs"] => (Action::CreateBlog, pr.facts(who, 0, 0)),
        ["dash", s, "posts"] => {
            let b = blog(pr, s);
            (Action::CreatePost { blog: b }, pr.facts(who, b, 0))
        }
        ["dash", s, "authors"] => {
            let b = blog(pr, s);
            let email = form.get("email").unwrap_or("").trim().to_ascii_lowercase();
            let user = pr.int(P::UserByEmail, &[Val::Text(email.as_bytes())]).unwrap_or(0) as u64;
            (Action::AddAuthor { blog: b, user }, pr.facts(who, b, 0))
        }
        ["dash", s, "authors", u, "remove"] => {
            let b = blog(pr, s);
            (Action::RemoveAuthor { blog: b, user: num(u) }, pr.facts(who, b, 0))
        }
        ["dash", s, "delete"] => {
            let b = blog(pr, s);
            (Action::DeleteBlog { blog: b }, pr.facts(who, b, 0))
        }
        ["edit", id] => {
            let p = num(id);
            let a = if form.get("action") == Some("publish") { Action::PublishPost { post: p } } else { Action::EditPost { post: p } };
            (a, pr.facts(who, 0, p))
        }
        ["edit", id, "publish" | "unpublish"] => (Action::PublishPost { post: num(id) }, pr.facts(who, 0, num(id))),
        ["edit", id, "delete"] => (Action::DeletePost { post: num(id) }, pr.facts(who, 0, num(id))),
        ["edit", id, "sync"] if body_len > 12 => (Action::EditPost { post: num(id) }, pr.facts(who, 0, num(id))),
        ["upload", id] => (Action::EditPost { post: num(id) }, pr.facts(who, 0, num(id))),
        ["comment", id] => (Action::Comment { post: num(id) }, pr.facts(who, 0, num(id))),
        ["comment", id, "delete"] => {
            let mut c = (0, 0);
            pr.q(P::CommentOf, &[Val::Int(num(id) as i64)], |r| c = (r.int(0) as u64, r.int(1) as u64));
            (Action::DeleteComment { post: c.0, author: c.1 }, pr.facts(who, 0, c.0))
        }
        ["like", id] => (Action::Like { post: num(id) }, pr.facts(who, 0, num(id))),
        _ => return None,
    })
}

impl App for Checked {
    fn handle(&mut self, req: &Request, cx: &mut Ctx, out: &mut Vec<u8>) -> bool {
        let target = String::from_utf8_lossy(req.target).to_string();
        let (path, _) = target.split_once('?').unwrap_or((&target, ""));
        let parts: Vec<&str> = path.split('/').skip(1).collect();
        let user = req.header(b"x-sim").and_then(|v| std::str::from_utf8(v).ok()?.parse().ok()).unwrap_or(usize::MAX);
        let sid = req.header(b"cookie").and_then(|c| c.windows(4).position(|w| w == b"sid=").map(|i| &c[i + 4..(i + 68).min(c.len())]));
        let faults = self.injected.get();
        let (asked, act, before, expect) = {
            let mut sh = self.sh.borrow_mut();
            let pr = &mut sh.probe;
            let who = pr.who(sid, cx.now_ms);
            let post = match parts.as_slice() {
                ["edit", id, ..] | ["live", id] | ["comments", id] => num(id),
                ["b", b, s] => pr.int(P::PostBySlug, &[Val::Text(b.as_bytes()), Val::Text(s.as_bytes())]).unwrap_or(0) as u64,
                _ => 0,
            };
            let form = crate::form::Form::parse(req.body).unwrap_or_else(|| crate::form::Form::parse(b"").expect("empty form"));
            let act = action(pr, who, req.method, &parts, &form, req.body.len());
            let before = act.as_ref().map(|_| pr.text(P::Fingerprint, &[]));
            let expect = expected(pr, who, req.method, &parts, &form, req.body).filter(|(a, f, _)| permits(f, *a));
            let what = format!("{} {}", String::from_utf8_lossy(req.method), target);
            let page = crate::form::Form::parse(target.split_once('?').map_or("", |x| x.1).as_bytes()).and_then(|q| q.num("page")).unwrap_or(1).clamp(1, 500);
            let feed = match (req.method, parts.as_slice()) {
                (b"GET", [""]) => Some((P::FeedAll, String::new(), page)),
                (b"GET", ["b", b]) => Some((P::FeedBlog, b.to_string(), page)),
                (b"GET", ["u", h]) => Some((P::FeedAuthor, h.to_string(), page)),
                _ => None,
            };
            let replace = match parts.as_slice() {
                ["edit", id] if req.method == b"POST" => form.get("body").map(|b| {
                    let before = pr.doc_text(num(id)).unwrap_or_default();
                    (num(id), b.replace("\r\n", "\n"), tokens(before.as_bytes()).into_iter().map(|t| t.2).collect())
                }),
                _ => None,
            };
            let image = match parts.as_slice() {
                ["img", k] if req.method == b"GET" => Some(k.to_string()),
                _ => None,
            };
            (Asked { user, who, what, post, dash: req.method == b"GET" && parts == ["dash"], feed, replace, image, parked_at: cx.mono_ms, doc_wait: parts.len() == 3 && parts[2] == "sync", me: crate::form::Form::parse(target.split_once('?').map_or("", |x| x.1).as_bytes()).and_then(|q| q.num("me")).unwrap_or(0) as u32 }, act, before, expect)
        };
        let keep = self.site.handle(req, cx, out);
        {
            let mut sh = self.sh.borrow_mut();
            let status = out.get(9..12).unwrap_or(b"000");
            // Not a path: 400.
            if (!req.target.is_ascii() || !req.target.starts_with(b"/")) && status != b"400" {
                sh.violation(asked.user, &asked.what, format!("a target that is not a path, answered {}", String::from_utf8_lossy(status)));
            }
            match parts.as_slice() {
                // A stored batch is well-formed (by the oracle's reading).
                ["edit", _, "sync"] if req.body.len() > 12 && status == b"200" && !well_formed(&req.body[12..]) => {
                    sh.violation(asked.user, &asked.what, "a malformed batch stored (the wire format's rules)".into());
                }
                // Only images are stored as images.
                ["upload", _] if status == b"200" && !is_image(req.body) => {
                    sh.violation(asked.user, &asked.what, "bytes that are not an image stored as one".into());
                }
                // A post's address, if given, is refused unless it is one.
                ["dash", _, "posts"] if req.method == b"POST" => {
                    let form = crate::form::Form::parse(req.body);
                    if let Some(s) = form.as_ref().and_then(|f| f.get("slug")).map(str::trim).filter(|s| !s.is_empty()) {
                        // (Refused by the form's check, or earlier: not signed in,
                        // no such blog. Not by the database, not accepted.)
                        if !slug_ok(s) && !matches!(status, b"400" | b"403" | b"404") {
                            sh.violation(asked.user, &asked.what, format!("a post address {s:?} that is not one, answered {}", String::from_utf8_lossy(status)));
                        }
                    }
                }
                _ => {}
            }
            // An uploaded image, still there, is served.
            if let Some(k) = asked.image.as_ref().filter(|k| sh.images.contains_key(*k)) {
                if status != b"200" && self.injected.get() == faults && sh.probe.int(P::ImageExists, &[Val::Text(k.as_bytes())]) == Some(1) {
                    sh.violation(asked.user, &asked.what, format!("an image that was uploaded (and is still there) answered {}", String::from_utf8_lossy(status)));
                }
            }
        }
        if let Ok(t) = std::env::var("APPSIM_TRACE") {
            if find(req.body, t.as_bytes()).is_some() || find(out, t.as_bytes()).is_some() {
                let stored = self.sh.borrow_mut().probe.int(P::Traced, &[Val::Text(t.as_bytes())]).unwrap_or(0);
                eprintln!("TRACE t={} conn {:x} {} {}: answer {} (park {}), faults during {}, batches holding it now {}", self.sh.borrow().now, cx.conn, String::from_utf8_lossy(req.method), target, String::from_utf8_lossy(out.get(9..12).unwrap_or(b"?")), cx.park, self.injected.get() - faults, stored);
            }
        }
        // A sync that stored operations: what they typed is acknowledged
        // now (when the server stores it, not when the client hears); and
        // the change, when, by which replica (waits on the post must wake).
        if let (["edit", id, "sync"], true) = (parts.as_slice(), req.body.len() > 12 && out.get(9..12) == Some(b"200")) {
            let pid = num(id);
            let mut sh = self.sh.borrow_mut();

            if let Some(pnum) = sh.pnum(pid) {
                for (k, _, t) in tokens(&req.body[12..]) {
                    if k == b't' {
                        sh.acked.push((pid, pnum, t));
                    }
                }
            }
        }
        {
            let mut sh = self.sh.borrow_mut();
            if let (Some((a, facts)), Some(before)) = (act, before) {
                let after = sh.probe.text(P::Fingerprint, &[]);
                // (A sync that stored something new: waits on the post must wake.)
                if let (["edit", id, "sync"], true) = (parts.as_slice(), after != before) {
                    let rep = u32::from_le_bytes(req.body[8..12].try_into().expect("4"));
                    sh.changed.push((num(id), rep, cx.mono_ms));
                }
                // THE WRITE BUDGET (spec/authz.rs): 30 writes in each minute's
                // window; the windows are fixed, so any 60 seconds overlap two
                // at most: no more than 60 writes that change content in any
                // 60 seconds. (Sound whatever the windows; a budget that stops
                // holding shows past it.)
                if after != before && !matches!(parts.as_slice(), ["edit", _, "sync"]) && asked.who != 0 {
                    let t = cx.now_ms;
                    let times = sh.budget.entry(asked.who).or_default();
                    times.push(t);
                    times.retain(|&x| x + 60_000 > t && x <= t);
                    if times.len() > 60 {
                        let n = times.len();
                        sh.violation(asked.user, &asked.what, format!("{n} writes within 60 seconds by user {} (the budget allows 30 a minute: 60 at most across two)", asked.who));
                    }
                }
                if after != before && !permits(&facts, a) {
                    sh.violation(asked.user, &asked.what, format!("changed content the law does not permit ({:?} by user {}, facts {:?})", DebugAction(a), asked.who, DebugFacts(facts)));
                }
            }
        }
        // THE OTHER SIDE: permitted, and no fault touched it: success.
        if let Some((a, _, allowed)) = expect {
            let mut status: u16 = std::str::from_utf8(out.get(9..12).unwrap_or(b"0")).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
            // (A Datastar answer is 200 even when refused: its notice says
            // which refusal, and that is judged as the status would be.)
            if req.header(b"datastar-request").is_some() && find(out, b"class=\"notice bad\"").is_some() {
                status = if find(out, b"a lot of changes").is_some() { 429 } else if find(out, b"Not allowed").is_some() { 403 } else { 500 };
            }
            let mut sh = self.sh.borrow_mut();
            // (A 429 for a write only when the user's budget is really spent.)
            let limit = status == 429 && ((req.method == b"GET" && req.header(b"datastar-request").is_none()) || sh.probe.int(P::Budget, &[Val::Int(asked.who as i64), Val::Int(cx.now_ms as i64)]).unwrap_or(0) >= 30);
            // (A sync refused as malformed is right only if its batch is: the
            // oracle decides that itself, applying it to the document as
            // stored, read here.)
            let bad_batch = |sh: &mut Shared| -> bool {
                let (Some(body), ["edit", id, "sync"]) = (req.body.get(12..), parts.as_slice()) else { return false };
                let rep = u32::from_le_bytes(req.body[8..12].try_into().expect("4"));
                if !decodes(body) || !well_formed(body) {
                    return true;
                }
                let mut i = 0;
                while i < body.len() {
                    let (op, n) = crate::crdt::decode(body, i).expect("decodes");
                    if let crate::crdt::Op::Ins { rep: r, .. } = op {
                        if r != rep {
                            return true;
                        }
                    }
                    i = n;
                }
                match sh.probe.doc(num(id)) {
                    Ok(mut d) => d.apply_batch(body, &mut ()).is_err(),
                    Err(_) => false,
                }
            };
            // (Input the oracle itself judges bad may be refused: not an
            // image, 415; not a post address, 400.)
            let bad_input = match parts.as_slice() {
                ["upload", _] => status == 415 && !is_image(req.body),
                // (Operations only under a replica number this writer was given.)
                ["edit", id, "sync"] if req.body.len() > 12 => {
                    let rep = u32::from_le_bytes(req.body[8..12].try_into().expect("4"));
                    status == 403 && sh.probe.int(P::RepOwner, &[Val::Int(num(id) as i64), Val::Int(rep as i64)]) != Some(asked.who as i64)
                }
                ["dash", _, "posts"] => status == 400 && crate::form::Form::parse(req.body).as_ref().and_then(|f| f.get("slug")).map(str::trim).is_some_and(|s| !s.is_empty() && !slug_ok(s)),
                _ => false,
            };
            let refused_ok = (allowed.contains(&status) && (status != 429 || limit)) || (status == 400 && bad_batch(&mut sh)) || bad_input;
            if !cx.park && self.injected.get() == faults && !(200..400).contains(&status) && !refused_ok {
                sh.violation(asked.user, &asked.what, format!("the law permits this ({:?} by user {}) and no fault was injected, but the answer was {status}", DebugAction(a), asked.who));
            }
        }
        if cx.park {
            self.parked.insert(cx.conn, asked);
        } else {
            check(&mut self.sh.borrow_mut(), &asked, out);
        }
        keep
    }

    fn gone(&mut self, conn: u64) {
        self.site.gone(conn);
        self.parked.remove(&conn);
    }

    fn ready(&mut self, now_ms: u64, answers: &mut Vec<(u64, Vec<u8>)>) {
        let n = answers.len();
        self.site.ready(now_ms, answers);
        for (conn, bytes) in &answers[n..] {
            if let Some(asked) = self.parked.remove(conn) {
                // PUSH, NOT POLLING: a wait is answered empty only when its
                // time is up (else it waits for a change).
                let body = find(bytes, b"\r\n\r\n").map_or(&bytes[..0], |e| &bytes[e + 4..]);
                if asked.doc_wait && bytes.get(9..12) == Some(b"200") && body.len() == 10 && body[0] == 0 && body[9] == 0 && now_ms < asked.parked_at + 3000 - 50 {
                    self.sh.borrow_mut().violation(asked.user, &asked.what, format!("a wait answered with nothing after {} ms, before its time (3000 ms)", now_ms - asked.parked_at));
                }
                // ...and a change by someone else, stored while it waited,
                // wakes it at once (not at the end of its time).
                if asked.doc_wait {
                    let mut sh = self.sh.borrow_mut();
                    let late = sh.changed.iter().find(|(p, r, t)| *p == asked.post && *r != asked.me && *t > asked.parked_at && now_ms > *t + 500).map(|x| x.2);
                    if let Some(t) = late {
                        sh.violation(asked.user, &asked.what, format!("a change to post {} at {t} ms woke this wait only {} ms later", asked.post, now_ms - t));
                    }
                }
                check(&mut self.sh.borrow_mut(), &asked, bytes);
            }
        }
    }
}

/// Checks an answer (a whole response) against the laws, as the database
/// is now.
fn check(sh: &mut Shared, a: &Asked, resp: &[u8]) {
    let Some(e) = find(resp, b"\r\n\r\n") else { return };
    let (head, body) = (&resp[..e], &resp[e + 4..]);
    let status: u16 = std::str::from_utf8(head.get(9..12).unwrap_or(b"0")).ok().and_then(|s| s.parse().ok()).unwrap_or(0);
    sh.answers += 1;
    let mut problems = vec![];
    for (kind, p, tok) in tokens(body) {
        let Some(pid) = sh.posts.get(p).copied().flatten() else {
            problems.push(format!("a token of post p{p}, which no writer made: {tok}"));
            continue;
        };
        let Some((blog, _, ident)) = sh.probe.post(pid) else {
            problems.push(format!("text of a deleted post (p{p}, id {pid}): {tok}"));
            continue;
        };
        if !ident.contains(&format!("p{p}.")) {
            problems.push(format!("text of post p{p} under post id {pid}, which is another post ({ident}): {tok}"));
            continue;
        }
        // THE LAW (spec/authz.rs ReadPost, through law.rs), on facts read here.
        let facts = sh.probe.facts(a.who, blog, pid);
        let member = facts.role.is_some();
        let may_read = permits(&facts, Action::ReadPost { post: pid });
        if kind == b't' {
            if a.post != 0 && a.post != pid {
                problems.push(format!("text of post {pid} in an answer about post {}: {tok}", a.post));
            }
            if !may_read {
                problems.push(format!("text of post {pid}, which the law says user {} may not read: {tok}", a.who));
            } else if !member && sh.probe.int(P::InBody, &[Val::Int(pid as i64), Val::Text(tok.as_bytes())]) != Some(1) {
                // (Beyond the law: readers see the published text, not the
                // edits since.)
                problems.push(format!("text of post {pid} not yet published, to user {}, not a member: {tok}", a.who));
            }
        } else {
            if sh.probe.int(P::CommentLive, &[Val::Int(pid as i64), Val::Text(tok.as_bytes())]).unwrap_or(0) == 0 {
                problems.push(format!("a comment on post {pid} that does not exist or was deleted: {tok}"));
            }
            if !may_read {
                problems.push(format!("a comment on post {pid}, which the law says user {} may not read: {tok}", a.who));
            }
        }
    }
    // A JSON answer a browser's JSON.parse would refuse.
    if find(head, b"application/json").is_some() && super::client::parse_json(body).is_none() {
        problems.push(format!("a JSON answer that does not parse: {:?}", String::from_utf8_lossy(&body[..body.len().min(120)])));
    }
    let html = find(head, b"text/html").is_some();
    if html {
        for b in bodies(body) {
            // (A deleted comment's placeholder is the template's, not rendered text.)
            if b == &b"<p class=\"muted\">[deleted]</p>"[..] {
                continue;
            }
            if let Err(e) = markup_ok(b) {
                problems.push(format!("rendered markup the law does not allow: {e}"));
            }
        }
        if a.who == 0 && status == 200 && find(body, b"action=\"/logout\"").is_some() {
            problems.push("a page shows someone signed in with no valid session (by the wall clock)".into());
        }
    }
    if sh.quiet && status >= 500 {
        problems.push(format!("a failure ({status}) with no faults injected"));
    }
    // The form replaced the text: the document is now exactly it; what it
    // replaced counts as deleted (not lost), what it says as acknowledged.
    if let (Some((pid, text, before)), 303) = (&a.replace, status) {
        // (Whatever was in the text, acknowledged or still on its way back
        // to its writer, is replaced: deleted, not lost.)
        for t in before {
            if !text.contains(t.as_str()) {
                sh.deleted.insert(t.clone());
            }
        }
        match sh.probe.doc_text(*pid) {
            Ok(t) if &t == text => {}
            Ok(t) => problems.push(format!("the form's text did not become the post's ({} bytes sent, {} there)", text.len(), t.len())),
            Err(e) => problems.push(format!("the post's document does not load after a form save: {e}")),
        }
        let pnum = sh.pnum(*pid);
        let acked = std::mem::take(&mut sh.acked);
        for (p, _, t) in &acked {
            if p == pid && !text.contains(t.as_str()) {
                sh.deleted.insert(t.clone());
            }
        }
        sh.acked = acked;
        if let Some(pnum) = pnum {
            for (k, _, t) in tokens(text.as_bytes()) {
                if k == b't' {
                    sh.acked.push((*pid, pnum, t));
                }
            }
        }
    }
    // An image is what was uploaded, byte for byte.
    if let (Some(k), 200) = (&a.image, status) {
        match sh.images.get(k) {
            Some(b) if b.as_slice() == body => {}
            Some(_) => problems.push(format!("image {k} is not what was uploaded")),
            None => problems.push(format!("image {k}, which nobody uploaded")),
        }
    }
    if let (Some((which, key, page)), 200) = (&a.feed, status) {
        if let Err(e) = feed_ok(sh, *which, key, *page, body) {
            problems.push(format!("a feed page wrong: {e}"));
        }
    }
    if a.dash && status == 200 && a.who == 0 {
        problems.push("the dashboard, with no valid session (by the wall clock)".into());
    }
    for pb in problems {
        sh.violation(a.user, &a.what, pb);
    }
}

// (Debug output for the laws' types, which do not derive it.)
struct DebugAction(Action);
impl std::fmt::Debug for DebugAction {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.0 {
            Action::CreateBlog => write!(f, "CreateBlog"),
            Action::EditBlog { blog } => write!(f, "EditBlog({blog})"),
            Action::DeleteBlog { blog } => write!(f, "DeleteBlog({blog})"),
            Action::AddAuthor { blog, user } => write!(f, "AddAuthor({blog}, {user})"),
            Action::RemoveAuthor { blog, user } => write!(f, "RemoveAuthor({blog}, {user})"),
            Action::CreatePost { blog } => write!(f, "CreatePost({blog})"),
            Action::EditPost { post } => write!(f, "EditPost({post})"),
            Action::PublishPost { post } => write!(f, "PublishPost({post})"),
            Action::DeletePost { post } => write!(f, "DeletePost({post})"),
            Action::ReadPost { post } => write!(f, "ReadPost({post})"),
            Action::ReadBlogAdmin { blog } => write!(f, "ReadBlogAdmin({blog})"),
            Action::Comment { post } => write!(f, "Comment({post})"),
            Action::DeleteComment { post, author } => write!(f, "DeleteComment({post}, {author})"),
            Action::Like { post } => write!(f, "Like({post})"),
        }
    }
}

struct DebugFacts(Facts);
impl std::fmt::Debug for DebugFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        let role = match self.0.role {
            Some(Role::Owner) => "owner",
            Some(Role::Author) => "author",
            None => "none",
        };
        let post = self.0.post.as_ref().map(|p| (p.id, p.blog, p.published));
        write!(f, "who {} blog {} role {role} post {post:?}", self.0.who, self.0.blog)
    }
}
