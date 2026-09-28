// SIMULATED USERS: people who sign up and sign in with passkeys, make
// blogs, invite and remove authors, write in the editor (a tab holds a
// replica of the post's CRDT and speaks the editor's sync protocol:
// sending its operations, a request waiting for others', kept edits in
// local storage), publish, read, comment, go offline, reload, restart the
// browser. Everything they choose comes from the seed. They find their
// way (their blogs, the published posts) through the oracle's own reading
// of the database, as a person would through the pages.
use super::client::{Passkey, Req, Resp};
use super::net::Net;
use super::oracle::{Probe, P};
use super::Shared;
use crate::crdt::Doc;
use crate::sim::Rng;
use crate::sys::sqlite::Val;
use std::collections::{BTreeMap, VecDeque};

/// A request on its way: its connection, when it went, what came back so far.
pub struct Flight {
    conn: usize,
    at: u64,
    got: Vec<u8>,
}

/// A finished request: its response, or None (reset, cut short, given up).
type Done = Option<Resp>;

impl Flight {
    fn go(net: &mut Net, r: Req) -> Flight {
        let at = net.now;
        Flight { conn: net.connect(r.wire()), at, got: vec![] }
    }

    /// The response once the request has ended.
    fn poll(&mut self, net: &mut Net) -> Option<Done> {
        let (b, ended, reset) = net.client_take(self.conn);
        self.got.extend_from_slice(&b);
        if !ended {
            return None;
        }
        Some(if reset { None } else { Resp::parse(&self.got) })
    }

    fn abort(&self, net: &mut Net) {
        net.client_close(self.conn);
    }
}

/// What a person is doing in the foreground.
enum Fg {
    Idle,
    Signup,
    RegOptions(String),
    Register(String, Passkey),
    LoginOptions,
    Login,
    NewPost(usize),
    Plain(&'static str),
    Read(u64),
    Live(u64, u32),
}

pub struct Tab {
    pub pid: u64,
    pub pnum: usize,
    pub rep: u32,
    pub doc: Doc,
    since: i64,
    /// Unsent batches (replica, operations), oldest first; the first
    /// `sending` are in the sync request on its way.
    batches: VecDeque<(u32, Vec<u8>)>,
    sending: usize,
    pub loaded: bool,
    pub dead: bool,
    page: Option<Flight>,
    sync: Option<Flight>,
    wait: Option<Flight>,
    send_at: u64,
    wait_at: u64,
    type_at: u64,
    stored_at: u64,
}

pub struct Person {
    pub idx: usize,
    pub email: String,
    pub key: Option<Passkey>,
    pub sid: Option<String>,
    pub uid: u64,
    fg: Fg,
    fl: Option<Flight>,
    next_at: u64,
    pub tabs: Vec<Tab>,
    /// Local storage: kept unsent batches by post.
    pub storage: BTreeMap<u64, Vec<(u32, Vec<u8>)>>,
    offline_until: u64,
    typed: u64,
}

pub struct Users {
    pub people: Vec<Person>,
    pub rng: Rng,
    /// Quiet: no new actions or typing (the end of a run); only syncing.
    pub quiet: bool,
    /// Counts, for the report.
    pub stats: BTreeMap<&'static str, u64>,
}

fn stat(s: &mut BTreeMap<&'static str, u64>, k: &'static str) {
    *s.entry(k).or_insert(0) += 1;
}

impl Users {
    pub fn new(n: usize, seed: u64) -> Users {
        let people = (0..n)
            .map(|i| Person {
                idx: i,
                email: format!("u{i}@sim.example"),
                key: None,
                sid: None,
                uid: 0,
                fg: Fg::Idle,
                fl: None,
                next_at: 0,
                tabs: vec![],
                storage: BTreeMap::new(),
                offline_until: 0,
                typed: 0,
            })
            .collect();
        Users { people, rng: Rng(seed ^ 0x5eed_0f_05e5), quiet: false, stats: BTreeMap::new() }
    }

    /// One step of time for everyone.
    pub fn step(&mut self, net: &mut Net, sh: &mut Shared) {
        for i in 0..self.people.len() {
            self.person(i, net, sh);
        }
    }

    fn person(&mut self, i: usize, net: &mut Net, sh: &mut Shared) {
        let now = net.now;
        let rng = &mut self.rng;
        let stats = &mut self.stats;
        let p = &mut self.people[i];
        // Offline: everything on its way is cut; nothing goes.
        if now < p.offline_until {
            return;
        }
        // THE FOREGROUND.
        if let Some(fl) = &mut p.fl {
            if let Some(done) = fl.poll(net) {
                p.fl = None;
                stat(stats, if done.is_some() { "answers" } else { "no answer" });
                foreground(p, done, net, sh, rng, stats);
            } else if now - fl.at > 45_000 {
                sh.violation(i, "a request", "no answer within 45 s".into());
                fl.abort(net);
                p.fl = None;
                p.fg = Fg::Idle;
            }
        } else if now >= p.next_at && !self.quiet {
            choose(p, net, sh, rng, stats);
        }
        // THE TABS.
        let quiet = self.quiet;
        for t in 0..p.tabs.len() {
            tab(p, t, net, sh, rng, stats, quiet);
        }
        p.tabs.retain(|t| !(t.dead && t.page.is_none() && t.sync.is_none() && t.wait.is_none()));
    }

    /// Everyone online; stop choosing new things (the end of a run).
    pub fn settle(&mut self) {
        self.quiet = true;
        for p in &mut self.people {
            p.offline_until = 0;
        }
    }

    /// Nothing left to send or answer.
    pub fn settled(&self) -> bool {
        self.people.iter().all(|p| p.fl.is_none() && p.tabs.iter().all(|t| t.dead || (t.loaded && t.batches.is_empty() && t.sync.is_none())))
    }
}

fn delay(rng: &mut Rng, lo: u64, hi: u64) -> u64 {
    lo + rng.below(hi - lo)
}

fn random_bytes(rng: &mut Rng) -> [u8; 32] {
    let mut n = [0u8; 32];
    for c in n.chunks_mut(8) {
        c.copy_from_slice(&rng.next().to_le_bytes());
    }
    n
}

/// Picks something to do.
fn choose(p: &mut Person, net: &mut Net, sh: &mut Shared, rng: &mut Rng, stats: &mut BTreeMap<&'static str, u64>) {
    let u = p.idx;
    p.next_at = net.now + delay(rng, 200, 4000);
    if p.key.is_none() {
        p.fg = Fg::Signup;
        let email = p.email.clone();
        let handle = format!("user{u}");
        p.fl = Some(Flight::go(net, Req::form(u, "/signup".into(), &None, &[("name", &format!("User {u}")), ("email", &email), ("handle", &handle)])));
        return;
    }
    if p.sid.is_none() {
        p.fg = Fg::LoginOptions;
        p.fl = Some(Flight::go(net, Req::form(u, "/passkey/login/options".into(), &None, &[])));
        return;
    }
    let sid = p.sid.clone();
    let mine = my_blogs(&mut sh.probe, p.uid);
    let posts: Vec<u64> = mine.iter().flat_map(|(b, _, _)| blog_posts(&mut sh.probe, *b)).collect();
    let published = published(&mut sh.probe);
    let roll = rng.below(100);
    let plain = |p: &mut Person, net: &mut Net, what: &'static str, r: Req| {
        p.fg = Fg::Plain(what);
        p.fl = Some(Flight::go(net, r));
    };
    match roll {
        // Open a tab on one of my posts.
        0..=14 if p.tabs.len() < 2 && !posts.is_empty() => {
            let pid = posts[rng.below(posts.len() as u64) as usize];
            if let Some(pnum) = sh.pnum(pid) {
                open_tab(p, pid, pnum, net);
                stat(stats, "tabs opened");
            }
        }
        // A new post (in one of my blogs), or a blog.
        15..=24 => {
            if mine.is_empty() || rng.below(6) == 0 {
                let slug = format!("b{}x{}", u, rng.below(1_000_000));
                plain(p, net, "blogs made", Req::form(u, "/blogs".into(), &sid, &[("title", &format!("Blog {slug}")), ("slug", &slug)]));
            } else {
                let (_, slug, _) = &mine[rng.below(mine.len() as u64) as usize];
                let pnum = sh.new_pnum();
                p.fg = Fg::NewPost(pnum);
                p.fl = Some(Flight::go(net, Req::form(u, format!("/dash/{slug}/posts"), &sid, &[("title", &format!("Post p{pnum}."))])));
            }
        }
        // Invite someone to a blog I own.
        25..=30 => {
            let owned: Vec<&(u64, String, i64)> = mine.iter().filter(|b| b.2 == 1).collect();
            let other = rng.below(sh.people as u64) as usize;
            if let Some((_, slug, _)) = owned.get(rng.below(owned.len().max(1) as u64) as usize).copied() {
                plain(p, net, "authors added", Req::form(u, format!("/dash/{slug}/authors"), &sid, &[("email", &format!("u{other}@sim.example"))]));
            }
        }
        // Remove an author from a blog I own.
        31..=33 => {
            let owned: Vec<&(u64, String, i64)> = mine.iter().filter(|b| b.2 == 1).collect();
            if let Some((b, slug, _)) = owned.get(rng.below(owned.len().max(1) as u64) as usize).copied() {
                let authors = members(&mut sh.probe, *b).into_iter().filter(|m| m.1 == 2).collect::<Vec<_>>();
                if !authors.is_empty() {
                    let (who, _) = authors[rng.below(authors.len() as u64) as usize];
                    plain(p, net, "authors removed", Req::form(u, format!("/dash/{slug}/authors/{who}/remove"), &sid, &[]));
                }
            }
        }
        // Publish (or update) one of my posts.
        34..=41 if !posts.is_empty() => {
            let pid = posts[rng.below(posts.len() as u64) as usize];
            if let Some(pnum) = sh.pnum(pid) {
                plain(p, net, "publishes", Req::form(u, format!("/edit/{pid}"), &sid, &[("title", &format!("Post p{pnum}.")), ("action", "publish")]));
            }
        }
        // Delete a post: often the newest (whose id comes next).
        42..=43 if !posts.is_empty() => {
            let pid = if rng.below(2) == 0 { *posts.iter().max().expect("some") } else { posts[rng.below(posts.len() as u64) as usize] };
            plain(p, net, "posts deleted", Req::form(u, format!("/edit/{pid}/delete"), &sid, &[]));
        }
        // Delete a blog I own (rarely).
        44 => {
            if let Some((_, slug, _)) = mine.iter().find(|b| b.2 == 1) {
                plain(p, net, "blogs deleted", Req::form(u, format!("/dash/{slug}/delete"), &sid, &[]));
            }
        }
        // Read a published post, then follow its comments live.
        45..=62 if !published.is_empty() => {
            let (pid, blog, slug) = &published[rng.below(published.len() as u64) as usize];
            p.fg = Fg::Read(*pid);
            p.fl = Some(Flight::go(net, Req::get(u, format!("/b/{blog}/{slug}"), &sid)));
        }
        // Comment on a published post.
        63..=72 if !published.is_empty() => {
            let (pid, _, _) = &published[rng.below(published.len() as u64) as usize];
            if let Some(pnum) = sh.pnum(*pid) {
                p.typed += 1;
                let body = format!("Nice. *cm{pnum}x{}q* [a link](https://example.com/x?a=1&b=2)", p.idx as u64 * 1_000_000 + p.typed);
                plain(p, net, "comments", Req::form(u, format!("/comment/{pid}"), &sid, &[("body", &body), ("parent", "0"), ("after", "0")]));
            }
        }
        // Delete one of my comments.
        73..=74 => {
            let mut cs = vec![];
            sh.probe.q(P::MyComments, &[Val::Int(p.uid as i64)], |r| cs.push(r.int(0)));
            if !cs.is_empty() {
                let c = cs[rng.below(cs.len() as u64) as usize];
                plain(p, net, "comments deleted", Req::form(u, format!("/comment/{c}/delete"), &sid, &[]));
            }
        }
        // The dashboard; the home page.
        75..=78 => plain(p, net, "dashboards", Req::get(u, "/dash".into(), &sid)),
        79..=80 => plain(p, net, "home pages", Req::get(u, "/".into(), &sid)),
        // Reload a tab (its unsent edits kept on the device, first).
        81..=84 if !p.tabs.is_empty() => {
            let t = rng.below(p.tabs.len() as u64) as usize;
            let (pid, pnum) = (p.tabs[t].pid, p.tabs[t].pnum);
            close_tab(p, t, net, true);
            open_tab(p, pid, pnum, net);
            stat(stats, "reloads");
        }
        // Close a tab.
        85..=86 if !p.tabs.is_empty() => {
            let t = rng.below(p.tabs.len() as u64) as usize;
            close_tab(p, t, net, true);
        }
        // The browser shut down (tabs gone, only what storage had kept).
        87 => {
            for t in (0..p.tabs.len()).rev() {
                close_tab(p, t, net, false);
            }
            stat(stats, "browser restarts");
        }
        // Offline for a while (what is on its way is cut).
        88..=90 => {
            p.offline_until = net.now + delay(rng, 500, 20_000);
            for t in &mut p.tabs {
                for f in [t.page.take(), t.sync.take(), t.wait.take()].into_iter().flatten() {
                    f.abort(net);
                }
                t.sending = 0;
                if !t.loaded {
                    t.dead = true;
                }
            }
            stat(stats, "offline spells");
        }
        // CURIOUS: try something that may not be allowed, on anyone's post,
        // blog or comment (the oracle judges what comes back, and any
        // change made).
        92..=99 => {
            curious(p, net, sh, rng);
            stat(stats, "curious attempts");
        }
        // Log out (rarely).
        91 => {
            plain(p, net, "logouts", Req::form(u, "/logout".into(), &sid, &[]));
            p.sid = None;
        }
        _ => {}
    }
}

/// Byte spans of whole tokens (" tk<p>x<n>q") in a text.
fn token_spans(t: &str) -> Vec<(usize, usize)> {
    let b = t.as_bytes();
    let mut v = vec![];
    let mut i = 0;
    while let Some(k) = t[i..].find(" tk") {
        let s = i + k;
        let mut j = s + 3;
        let d1 = b[j..].iter().take_while(|c| c.is_ascii_digit()).count();
        j += d1;
        if d1 > 0 && b.get(j) == Some(&b'x') {
            let d2 = b[j + 1..].iter().take_while(|c| c.is_ascii_digit()).count();
            if d2 > 0 && b.get(j + 1 + d2) == Some(&b'q') {
                v.push((s, j + 2 + d2));
                i = j + 2 + d2;
                continue;
            }
        }
        i = s + 1;
    }
    v
}

/// Something that may be refused: reading or changing others' things.
fn curious(p: &mut Person, net: &mut Net, sh: &mut Shared, rng: &mut Rng) {
    let (u, sid) = (p.idx, p.sid.clone());
    let mut posts = vec![];
    sh.probe.q(P::AllPosts, &[], |r| posts.push((r.int(0) as u64, r.text(1).to_string(), r.text(2).to_string())));
    let mut blogs = vec![];
    sh.probe.q(P::AllBlogs, &[], |r| blogs.push((r.int(0) as u64, r.text(1).to_string())));
    let mut comments = vec![];
    sh.probe.q(P::AllComments, &[], |r| comments.push(r.int(0)));
    if posts.is_empty() || blogs.is_empty() {
        return;
    }
    let (pid, blog, slug) = posts[rng.below(posts.len() as u64) as usize].clone();
    let (bid, bslug) = blogs[rng.below(blogs.len() as u64) as usize].clone();
    let pnum = sh.pnum(pid).unwrap_or(0);
    let r = match rng.below(12) {
        0 => Req::get(u, format!("/b/{blog}/{slug}"), &sid),
        1 => Req::get(u, format!("/edit/{pid}"), &sid),
        2 => Req::get(u, format!("/edit/{pid}/preview"), &sid),
        3 => Req::get(u, format!("/live/{pid}?n=0&after=0"), &sid),
        4 => {
            // (Reading others' text through sync, with no operations.)
            let mut body = 0u64.to_le_bytes().to_vec();
            body.extend_from_slice(&2u32.to_le_bytes());
            Req::bytes(u, format!("/edit/{pid}/sync?me=2"), &sid, body)
        }
        5 => Req::form(u, format!("/edit/{pid}"), &sid, &[("title", &format!("Post p{pnum}.")), ("action", "publish")]),
        6 => Req::form(u, format!("/edit/{pid}/unpublish"), &sid, &[]),
        7 => Req::form(u, format!("/edit/{pid}/delete"), &sid, &[]),
        8 => {
            let other = rng.below(sh.people as u64) as usize;
            Req::form(u, format!("/dash/{bslug}/authors"), &sid, &[("email", &format!("u{other}@sim.example"))])
        }
        9 => {
            let mut ms = vec![];
            sh.probe.q(P::Members, &[Val::Int(bid as i64)], |r| ms.push(r.int(0)));
            let who = ms.get(rng.below(ms.len().max(1) as u64) as usize).copied().unwrap_or(1);
            Req::form(u, format!("/dash/{bslug}/authors/{who}/remove"), &sid, &[])
        }
        10 if !comments.is_empty() => Req::form(u, format!("/comment/{}/delete", comments[rng.below(comments.len() as u64) as usize]), &sid, &[]),
        _ => Req::form(u, format!("/comment/{pid}"), &sid, &[("body", &format!("Curious *cm{pnum}x{}q*", u as u64 * 1_000_000 + 999_999)), ("parent", "0"), ("after", "0")]),
    };
    p.fg = Fg::Plain("curious answered");
    p.fl = Some(Flight::go(net, r));
}

fn my_blogs(pr: &mut Probe, uid: u64) -> Vec<(u64, String, i64)> {
    let mut v = vec![];
    pr.q(P::MyBlogs, &[Val::Int(uid as i64)], |r| v.push((r.int(0) as u64, r.text(1).to_string(), r.int(2))));
    v
}

fn blog_posts(pr: &mut Probe, b: u64) -> Vec<u64> {
    let mut v = vec![];
    pr.q(P::BlogPosts, &[Val::Int(b as i64)], |r| v.push(r.int(0) as u64));
    v
}

fn published(pr: &mut Probe) -> Vec<(u64, String, String)> {
    let mut v = vec![];
    pr.q(P::Published, &[], |r| v.push((r.int(0) as u64, r.text(1).to_string(), r.text(2).to_string())));
    v
}

fn members(pr: &mut Probe, b: u64) -> Vec<(u64, i64)> {
    let mut v = vec![];
    pr.q(P::Members, &[Val::Int(b as i64)], |r| v.push((r.int(0) as u64, r.int(1))));
    v
}

/// A foreground request finished.
fn foreground(p: &mut Person, done: Done, net: &mut Net, sh: &mut Shared, rng: &mut Rng, stats: &mut BTreeMap<&'static str, u64>) {
    let u = p.idx;
    let fg = std::mem::replace(&mut p.fg, Fg::Idle);
    let Some(r) = done else { return };
    // A signed-in request sent back to log in: the session has ended.
    if r.status == 303 && r.header("location").is_some_and(|l| l.starts_with("/login")) {
        p.sid = None;
        return;
    }
    if r.status == 403 && p.sid.is_some() && matches!(fg, Fg::Plain(_) | Fg::NewPost(_)) {
        // (Maybe a lost session, maybe a refusal: the next action finds out.)
        if rng.below(2) == 0 {
            p.sid = None;
        }
    }
    match fg {
        Fg::Signup => {
            // Test mode (no mail): on to making a passkey, with the link's token.
            if r.status == 303 {
                if let Some(t) = r.header("location").and_then(|l| l.split("t=").nth(1).map(|t| t.split('&').next().unwrap_or("").to_string())) {
                    p.fg = Fg::RegOptions(t.clone());
                    p.fl = Some(Flight::go(net, Req::form(u, "/passkey/register/options".into(), &None, &[("t", &t)])));
                }
            }
        }
        Fg::RegOptions(t) => {
            if let Some(ch) = r.json("challenge").filter(|_| r.status == 200) {
                let key = Passkey::new(&mut || rng.next());
                let (cd, att) = key.register(&ch);
                p.fg = Fg::Register(t.clone(), key);
                p.fl = Some(Flight::go(net, Req::form(u, "/passkey/register".into(), &None, &[("t", &t), ("cd", &cd), ("att", &att)])));
            }
        }
        Fg::Register(_, key) => {
            if r.status == 200 {
                if let Some(s) = r.sid() {
                    p.sid = Some(s);
                    p.key = Some(key);
                    p.uid = sh.probe.int(P::UserByEmail, &[Val::Text(p.email.as_bytes())]).unwrap_or(0) as u64;
                    stat(stats, "sign-ups");
                }
            }
        }
        Fg::LoginOptions => {
            if let (Some(ch), Some(key)) = (r.json("challenge").filter(|_| r.status == 200), p.key.as_mut()) {
                let nonce = random_bytes(rng);
                if let Some((id, cd, ad, sig)) = key.assert(&ch, nonce) {
                    p.fg = Fg::Login;
                    p.fl = Some(Flight::go(net, Req::form(u, "/passkey/login".into(), &None, &[("id", &id), ("cd", &cd), ("ad", &ad), ("sig", &sig)])));
                }
            }
        }
        Fg::Login => {
            if r.status == 200 {
                p.sid = r.sid();
                stat(stats, "logins");
            }
        }
        Fg::NewPost(pnum) => {
            if r.status == 303 {
                if let Some(pid) = r.header("location").and_then(|l| l.strip_prefix("/edit/").and_then(|x| x.parse::<u64>().ok())) {
                    sh.made(pnum, pid);
                    stat(stats, "posts");
                    if p.tabs.len() < 3 {
                        open_tab(p, pid, pnum, net);
                    }
                }
            }
        }
        Fg::Read(pid) => {
            // Then follow its comments, live (the page's request, waiting).
            if r.status == 200 && rng.below(3) == 0 {
                let after = r.attr("data-signals:cafter").and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
                p.fg = Fg::Live(pid, 1 + rng.below(4) as u32);
                p.fl = Some(Flight::go(net, Req::get(u, format!("/live/{pid}?n=0&after={after}"), &p.sid)));
            }
        }
        Fg::Live(pid, left) => {
            let t = r.text();
            let after = t.split("\"cafter\": ").nth(1).and_then(|x| x.split(|c: char| !c.is_ascii_digit()).next()).and_then(|x| x.parse::<u64>().ok());
            if r.status == 200 && left > 1 {
                if let Some(a) = after {
                    p.fg = Fg::Live(pid, left - 1);
                    p.fl = Some(Flight::go(net, Req::get(u, format!("/live/{pid}?n={left}&after={a}"), &p.sid)));
                }
            }
        }
        Fg::Plain(what) => {
            if r.status < 400 {
                stat(stats, what);
            }
        }
        Fg::Idle => {}
    }
}

fn open_tab(p: &mut Person, pid: u64, pnum: usize, net: &mut Net) {
    let page = Flight::go(net, Req::get(p.idx, format!("/edit/{pid}"), &p.sid));
    p.tabs.push(Tab {
        pid,
        pnum,
        rep: 0,
        doc: Doc::new(),
        since: 0,
        batches: VecDeque::new(),
        sending: 0,
        loaded: false,
        dead: false,
        page: Some(page),
        sync: None,
        wait: None,
        send_at: 0,
        wait_at: 0,
        type_at: net.now + 500,
        stored_at: net.now,
    });
}

/// Keeps a tab's unsent edits on the device (as the editor does at most
/// every second, and when the page is hidden or closed).
fn store(p: &mut Person, t: usize) {
    let tab = &p.tabs[t];
    if !tab.loaded {
        return;
    }
    let pid = tab.pid;
    let unsent: Vec<(u32, Vec<u8>)> = tab.batches.iter().cloned().collect();
    // (Other tabs on the same post keep theirs: one key per post, as in
    // the editor, so the last one stored wins.)
    if unsent.is_empty() {
        p.storage.remove(&pid);
    } else {
        p.storage.insert(pid, unsent);
    }
}

/// Closes a tab (keep: its unsent edits stored first; else only what was
/// stored before stays).
fn close_tab(p: &mut Person, t: usize, net: &mut Net, keep: bool) {
    if keep {
        store(p, t);
    }
    let tab = &mut p.tabs[t];
    for f in [tab.page.take(), tab.sync.take(), tab.wait.take()].into_iter().flatten() {
        f.abort(net);
    }
    p.tabs.remove(t);
}

/// A sync answer: a snapshot (the first), batches; since moves on.
/// Answers whether more waits.
fn take(tab: &mut Tab, r: &Resp, sh: &mut Shared, u: usize) -> Option<bool> {
    let b = &r.body;
    if b.len() < 10 {
        sh.violation(u, "sync", format!("a sync answer too short ({} bytes)", b.len()));
        return None;
    }
    let seq = i64::from_le_bytes(b[1..9].try_into().expect("8"));
    let more = b[9] == 1;
    let mut at = 10;
    if b[0] == 1 {
        let n = u32::from_le_bytes(b[10..14].try_into().ok()?) as usize;
        match Doc::load(&b[14..14 + n]) {
            Ok(d) => tab.doc = d,
            Err(e) => sh.violation(u, "sync", format!("a snapshot the CRDT refuses: {e:?}")),
        }
        at = 14 + n;
    }
    if let Err(e) = tab.doc.apply_batch(&b[at..], &mut ()) {
        sh.violation(u, "sync", format!("operations the CRDT refuses: {e:?}"));
    }
    tab.since = tab.since.max(seq);
    Some(more)
}

/// One step of a tab: its page, sending, waiting, typing, storing.
fn tab(p: &mut Person, t: usize, net: &mut Net, sh: &mut Shared, rng: &mut Rng, stats: &mut BTreeMap<&'static str, u64>, quiet: bool) {
    let now = net.now;
    let u = p.idx;
    let sid = p.sid.clone();
    // The page: the replica number, then the first sync with what was kept.
    if let Some(f) = &mut p.tabs[t].page {
        let Some(done) = f.poll(net) else { return };
        let tab = &mut p.tabs[t];
        tab.page = None;
        match done.as_ref().filter(|r| r.status == 200).and_then(|r| r.attr("data-rep")).and_then(|r| r.parse::<u32>().ok()) {
            Some(rep) => {
                tab.rep = rep;
                if let Some(kept) = p.storage.get(&tab.pid) {
                    tab.batches = kept.iter().cloned().collect();
                }
            }
            None => {
                tab.dead = true;
                return;
            }
        }
    }
    let tab = &mut p.tabs[t];
    if tab.dead {
        return;
    }
    // Answers.
    if let Some(f) = &mut tab.sync {
        if let Some(done) = f.poll(net) {
            tab.sync = None;
            let n = std::mem::replace(&mut tab.sending, 0);
            match done {
                Some(r) if r.status == 200 => {
                    let more = take(tab, &r, sh, u);
                    tab.batches.drain(..n.min(tab.batches.len()));
                    if !tab.loaded && more == Some(false) {
                        // Loaded: the kept edits go into the document too.
                        for (_, ops) in &tab.batches {
                            let _ = tab.doc.apply_batch(ops, &mut ());
                        }
                        tab.loaded = true;
                    }
                    tab.send_at = now + if more == Some(true) { 0 } else { 100 };
                }
                Some(r) if r.status == 403 || r.status == 404 => {
                    // (Logged out, removed, or the post is gone: the edits stay kept.)
                    tab.dead = true;
                    stat(stats, "tabs refused");
                    return;
                }
                _ => tab.send_at = now + delay(rng, 500, 2500),
            }
        } else if now - f.at > 45_000 {
            sh.violation(u, "sync", "no answer within 45 s".into());
            f.abort(net);
            tab.sync = None;
            tab.sending = 0;
        }
    }
    if let Some(f) = &mut tab.wait {
        if let Some(done) = f.poll(net) {
            tab.wait = None;
            match done {
                Some(r) if r.status == 200 => {
                    take(tab, &r, sh, u);
                    stat(stats, "waits answered");
                }
                Some(r) if r.status == 403 || r.status == 404 => {
                    tab.dead = true;
                    return;
                }
                _ => tab.wait_at = now + delay(rng, 1000, 3000),
            }
        } else if now - f.at > 45_000 {
            sh.violation(u, "wait", "a waiting sync not answered within 45 s".into());
            f.abort(net);
            tab.wait = None;
        }
    }
    // Send: the first sync (loading), or edits.
    if tab.sync.is_none() && (!tab.loaded || !tab.batches.is_empty()) && now >= tab.send_at {
        let r0 = tab.batches.front().map_or(tab.rep, |b| b.0);
        let mut body = (tab.since as u64).to_le_bytes().to_vec();
        body.extend_from_slice(&r0.to_le_bytes());
        let mut n = 0;
        for (r, ops) in &tab.batches {
            if *r != r0 || body.len() + ops.len() > 4 << 20 {
                break;
            }
            body.extend_from_slice(ops);
            n += 1;
        }
        tab.sending = n;
        tab.sync = Some(Flight::go(net, Req::bytes(u, format!("/edit/{}/sync?me={}", tab.pid, tab.rep), &sid, body)));
    }
    // Wait for others' changes.
    if tab.loaded && tab.wait.is_none() && now >= tab.wait_at {
        let mut body = (tab.since as u64).to_le_bytes().to_vec();
        body.extend_from_slice(&tab.rep.to_le_bytes());
        tab.wait = Some(Flight::go(net, Req::bytes(u, format!("/edit/{}/sync?wait=1&me={}", tab.pid, tab.rep), &sid, body)));
    }
    // Type: a token of this post somewhere, or delete a little.
    if tab.loaded && !quiet && now >= tab.type_at {
        tab.type_at = now + delay(rng, 30, 2500);
        let len = tab.doc.len16();
        let mut out = vec![];
        // (Deleting whole tokens only: cutting through them could splice
        // two halves into what reads as another post's token.)
        let text = tab.doc.text();
        let toks: Vec<(usize, usize)> = token_spans(&text);
        let r = if !toks.is_empty() && rng.below(4) == 0 {
            let (a, b) = toks[rng.below(toks.len() as u64) as usize];
            let pos = text[..a].encode_utf16().count() as u64;
            let del = text[a..b].encode_utf16().count() as u64;
            tab.doc.edit(tab.rep, pos, del, "", &mut out)
        } else {
            p.typed += 1;
            let fill = ["é", "日本", "", " and", ""][rng.below(5) as usize];
            let ins = format!(" tk{}x{}q{fill}", tab.pnum, p.idx as u64 * 1_000_000 + p.typed);
            let pos = rng.below(len + 1);
            tab.doc.edit(tab.rep, pos, 0, &ins, &mut out)
        };
        match r {
            Ok(()) if !out.is_empty() => {
                tab.batches.push_back((tab.rep, out));
                tab.send_at = tab.send_at.min(now + 100);
                stat(stats, "edits");
            }
            Ok(()) => {}
            Err(e) => sh.violation(u, "edit", format!("a local edit the CRDT refuses: {e:?}")),
        }
    }
    if now - tab.stored_at >= 1000 {
        tab.stored_at = now;
        store(p, t);
    }
}
