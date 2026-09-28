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

/// A request on its way: its connection, when it went, what came back so
/// far; the user, and whether the connection is kept after.
pub struct Flight {
    conn: usize,
    at: u64,
    got: Vec<u8>,
    user: usize,
    keep: bool,
    head: bool,
}

/// A finished request: its response, or None (reset, cut short, given up).
type Done = Option<Resp>;

impl Flight {
    fn go(net: &mut Net, mut r: Req) -> Flight {
        let at = net.now;
        let (user, keep) = (r.user, net.keeps.get(r.user).copied().unwrap_or(false));
        r.keep = keep;
        let head = r.method == "HEAD";
        Flight { conn: net.request(user, r.wire(), keep), at, got: vec![], user, keep, head }
    }

    /// The response once it is whole (a kept connection goes back to be
    /// used again), or the request has ended without one.
    fn poll(&mut self, net: &mut Net) -> Option<Done> {
        let (b, ended, reset) = net.client_take(self.conn);
        self.got.extend_from_slice(&b);
        if self.keep {
            if let Some(n) = super::client::whole(&self.got, self.head) {
                let r = Resp::parse(&self.got[..n]);
                // (Kept for the next request, unless the server says it
                // closes it: then the client closes it too, as a browser does.)
                if n == self.got.len() && !r.as_ref().and_then(|r| r.header("connection")).is_some_and(|v| v.eq_ignore_ascii_case("close")) {
                    net.release(self.user, self.conn);
                } else {
                    net.client_close(self.conn);
                }
                return Some(r);
            }
        }
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
    FinalDash,
    Recover,
    Verify(String),
    Upload(Vec<u8>),
    NewPost(usize),
    Plain(&'static str),
    Read(u64),
    Live(u64, u32),
}

/// A local edit's operations, its replica, and the tokens it typed (once
/// the server acknowledges it, they must never be lost).
#[derive(Clone)]
pub struct Batch {
    rep: u32,
    ops: Vec<u8>,
    toks: Vec<String>,
}

pub struct Tab {
    pub pid: u64,
    pub pnum: usize,
    pub rep: u32,
    pub doc: Doc,
    since: i64,
    /// Unsent batches, oldest first; the first `sending` are in the sync
    /// request on its way.
    batches: VecDeque<Batch>,
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
    /// Its first edit is a novel pasted in (a post begun by pasting).
    novel: bool,
}

pub struct Person {
    pub idx: usize,
    pub email: String,
    pub key: Option<Passkey>,
    /// A passkey made while its registration's answer was lost: the device
    /// keeps it (the account may exist); tried at the next sign-in.
    pending: Option<Passkey>,
    /// The key being tried is that one (a refusal: it was never stored).
    tentative: bool,
    /// The end of a run: 1 signing in again, 2 done (signed in, dashboard shown).
    pub last: u8,
    /// The passkey is gone (a new device): recover by email.
    pub lost: bool,
    /// The last text sent by form, by post.
    last_form: BTreeMap<u64, String>,
    /// Pages to look at next, in order (before anything else).
    then: VecDeque<String>,
    /// Writes still to make at once (a burst); recoveries still to ask for
    /// one address (a mail flood).
    burst: u32,
    flood: u32,
    flood_to: String,
    pub sid: Option<String>,
    pub uid: u64,
    fg: Fg,
    fl: Option<Flight>,
    next_at: u64,
    pub tabs: Vec<Tab>,
    /// Local storage: kept unsent batches by post.
    pub storage: BTreeMap<u64, Vec<Batch>>,
    offline_until: u64,
    typed: u64,
}

impl Person {
    /// (For reports: where this person's sign-in stands.)
    pub fn describe(&self) -> String {
        let fg = match &self.fg {
            Fg::Idle => "idle", Fg::Signup => "signing up", Fg::RegOptions(_) => "asking to register", Fg::Register(..) => "registering",
            Fg::LoginOptions => "asking to log in", Fg::Login => "logging in", Fg::FinalDash => "opening the dashboard",
            Fg::Recover => "recovering", Fg::Verify(_) => "opening the link", Fg::Upload(_) => "uploading", Fg::NewPost(_) => "making a post",
            Fg::Plain(w) => w, Fg::Read(_) => "reading", Fg::Live(..) => "following comments",
        };
        format!("user {}: {fg}, passkey {}, pending {}, lost {}, signed in {}, request on its way {}", self.idx, self.key.is_some(), self.pending.is_some(), self.lost, self.sid.is_some(), self.fl.is_some())
    }
}

pub struct Users {
    pub people: Vec<Person>,
    pub rng: Rng,
    /// How often each action is chosen (ACTS, scaled by the seed's swarm),
    /// and pastes of a large block, per thousand edits.
    pub weights: Vec<u32>,
    pub paste: u64,
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
                pending: None,
                tentative: false,
                last: 0,
                lost: false,
                last_form: BTreeMap::new(),
                burst: 0,
                then: VecDeque::new(),
                flood: 0,
                flood_to: String::new(),
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
        Users { people, rng: Rng(seed ^ 0x5eed_0f_05e5), weights: ACTS.iter().map(|a| a.1).collect(), paste: 3, quiet: false, stats: BTreeMap::new() }
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
        let weights = &self.weights;
        let paste = self.paste;
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
            choose(p, net, sh, rng, stats, weights);
        } else if self.quiet && p.last == 1 && now >= p.next_at {
            // THE LAST ROUND: faults off, everyone signs up (if they never
            // could) or signs in again, then opens their dashboard.
            p.next_at = now + 500;
            sign_in(p, net, sh, rng);
        }
        // THE TABS.
        let quiet = self.quiet;
        for t in 0..p.tabs.len() {
            tab(p, t, net, sh, rng, stats, quiet, paste);
        }
        p.tabs.retain(|t| !(t.dead && t.page.is_none() && t.sync.is_none() && t.wait.is_none()));
    }

    /// Everyone online; stop choosing new things (the end of a run); then
    /// everyone signs in again.
    pub fn settle(&mut self) {
        self.quiet = true;
        for p in &mut self.people {
            p.offline_until = 0;
            p.last = 1;
        }
    }

    /// Every browser shut: what is on its way is cut, tabs closed.
    pub fn shut(&mut self, net: &mut Net) {
        for p in &mut self.people {
            if let Some(f) = p.fl.take() {
                f.abort(net);
            }
            for t in &mut p.tabs {
                for f in [t.page.take(), t.sync.take(), t.wait.take()].into_iter().flatten() {
                    f.abort(net);
                }
            }
            p.tabs.clear();
        }
        net.close_idle();
    }

    /// Nothing left to send or answer, and everyone has signed in again.
    pub fn settled(&self) -> bool {
        self.people.iter().all(|p| p.last == 2 && p.fl.is_none() && p.tabs.iter().all(|t| t.dead || (t.loaded && t.batches.is_empty() && t.sync.is_none())))
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
/// What a person may do, and how often by default (swarm testing scales
/// these per seed: some to nothing, some up).
pub const ACTS: &[(&str, u32)] = &[
    ("open a tab", 15), ("a new post or blog", 10), ("invite", 6), ("remove an author", 3), ("publish", 8),
    ("delete a post", 2), ("delete a blog", 1), ("read", 18), ("comment", 10), ("delete a comment", 2),
    ("dashboard", 4), ("feeds", 4), ("reload a tab", 4), ("close a tab", 2), ("restart the browser", 1),
    ("go offline", 3), ("log out", 1), ("curious", 8), ("like", 5), ("reply", 4), ("more comments", 2),
    ("upload an image", 3), ("see an image", 2), ("save by form", 2), ("lose the passkey", 1), ("unpublish", 1),
    ("wander", 6), ("publish a draft", 2), ("bad sign-up", 2), ("burst", 1), ("forge a batch", 2), ("mail flood", 2),
];

/// Picks something to do.
fn choose(p: &mut Person, net: &mut Net, sh: &mut Shared, rng: &mut Rng, stats: &mut BTreeMap<&'static str, u64>, weights: &[u32]) {
    let u = p.idx;
    p.next_at = net.now + delay(rng, 200, 4000);
    if p.key.is_none() || p.sid.is_none() {
        sign_in(p, net, sh, rng);
        return;
    }
    let sid = p.sid.clone();
    if let Some(path) = p.then.pop_front() {
        p.next_at = net.now + 50;
        p.fg = Fg::Plain("looked again");
        // (A queued "…#" is the action itself: a POST.)
        let r = match path.strip_suffix('#') {
            Some(action) => Req::form(u, action.to_string(), &sid, &[]),
            None => Req::get(u, path, &sid),
        };
        p.fl = Some(Flight::go(net, r));
        return;
    }
    if p.flood > 0 {
        p.flood -= 1;
        p.next_at = net.now + 20;
        p.fg = Fg::Plain("flood requests");
        let to = p.flood_to.clone();
        p.fl = Some(Flight::go(net, if to.starts_with("fresh") {
            let h = to.split('@').next().unwrap_or("fresh").to_string();
            Req::form(u, "/signup".into(), &None, &[("name", "Fresh"), ("email", &to), ("handle", &h)])
        } else {
            Req::form(u, "/recover".into(), &None, &[("email", &to)])
        }));
        return;
    }
    if p.burst > 0 {
        // (A burst: likes and images, one after another, as fast as answers come.)
        p.burst -= 1;
        p.next_at = net.now + 20;
        let pubs = published(&mut sh.probe);
        let mine: Vec<u64> = my_blogs(&mut sh.probe, p.uid).iter().flat_map(|(b, _, _)| blog_posts(&mut sh.probe, *b)).collect();
        if rng.below(2) == 0 && !mine.is_empty() {
            let pid = mine[rng.below(mine.len() as u64) as usize];
            let mut img = b"GIF89a".to_vec();
            img.extend((0..32).map(|_| rng.next() as u8));
            p.fg = Fg::Upload(img.clone());
            p.fl = Some(Flight::go(net, Req::bytes(u, format!("/upload/{pid}"), &sid, img)));
        } else if let Some((pid, _, _)) = pubs.get(rng.below(pubs.len().max(1) as u64) as usize) {
            p.fg = Fg::Plain("burst writes");
            p.fl = Some(Flight::go(net, Req::form(u, format!("/like/{pid}"), &sid, &[("on", if p.burst % 2 == 0 { "1" } else { "0" })])));
        }
        return;
    }
    let mine = my_blogs(&mut sh.probe, p.uid);
    let posts: Vec<u64> = mine.iter().flat_map(|(b, _, _)| blog_posts(&mut sh.probe, *b)).collect();
    let published = published(&mut sh.probe);
    let total: u32 = weights.iter().sum();
    if total == 0 {
        return;
    }
    let mut roll = rng.below(total as u64) as u32;
    let mut act = 0;
    while roll >= weights[act] {
        roll -= weights[act];
        act += 1;
    }
    let plain = |p: &mut Person, net: &mut Net, what: &'static str, r: Req| {
        p.fg = Fg::Plain(what);
        p.fl = Some(Flight::go(net, r));
    };
    let pick = |rng: &mut Rng, n: usize| rng.below(n as u64) as usize;
    match ACTS[act].0 {
        // (Often the blog's newest post: co-authors join a post just begun.)
        "open a tab" if p.tabs.len() < 2 && !posts.is_empty() => {
            let pid = if rng.below(2) == 0 { *posts.iter().max().expect("some") } else { posts[pick(rng, posts.len())] };
            if let Some(pnum) = sh.pnum(pid) {
                open_tab(p, pid, pnum, net);
                stat(stats, "tabs opened");
            }
        }
        "a new post or blog" => {
            if mine.is_empty() || rng.below(6) == 0 {
                let slug = format!("b{}x{}", u, rng.below(1_000_000));
                // (Sometimes no address: the server makes one of the title.)
                let r = if rng.below(3) == 0 { Req::form(u, "/blogs".into(), &sid, &[("title", &format!("Blog {slug}"))]) } else { Req::form(u, "/blogs".into(), &sid, &[("title", &format!("Blog {slug}")), ("slug", &slug)]) };
                plain(p, net, "blogs made", r);
            } else {
                let (_, slug, _) = &mine[pick(rng, mine.len())];
                let pnum = sh.new_pnum();
                p.fg = Fg::NewPost(pnum);
                p.fl = Some(Flight::go(net, Req::form(u, format!("/dash/{slug}/posts"), &sid, &[("title", &format!("Post p{pnum}."))])));
            }
        }
        "invite" => {
            let owned: Vec<&(u64, String, i64)> = mine.iter().filter(|b| b.2 == 1).collect();
            let other = rng.below(sh.people as u64) as usize;
            if let Some((_, slug, _)) = owned.get(rng.below(owned.len().max(1) as u64) as usize).copied() {
                let ds = rng.below(2) == 0;
                plain(p, net, "authors added", Req::form(u, format!("/dash/{slug}/authors"), &sid, &[("email", &format!("u{other}@sim.example"))]).datastar(ds));
            }
        }
        "remove an author" => {
            let owned: Vec<&(u64, String, i64)> = mine.iter().filter(|b| b.2 == 1).collect();
            if let Some((b, slug, _)) = owned.get(rng.below(owned.len().max(1) as u64) as usize).copied() {
                let authors = members(&mut sh.probe, *b).into_iter().filter(|m| m.1 == 2).collect::<Vec<_>>();
                if !authors.is_empty() {
                    let (who, _) = authors[pick(rng, authors.len())];
                    plain(p, net, "authors removed", Req::form(u, format!("/dash/{slug}/authors/{who}/remove"), &sid, &[]));
                }
            }
        }
        "publish" if !posts.is_empty() => {
            let pid = posts[pick(rng, posts.len())];
            if let Some(pnum) = sh.pnum(pid) {
                plain(p, net, "publishes", Req::form(u, format!("/edit/{pid}"), &sid, &[("title", &format!("Post p{pnum}.")), ("action", "publish")]));
            }
        }
        "unpublish" if !posts.is_empty() => {
            let pid = posts[pick(rng, posts.len())];
            plain(p, net, "unpublishes", Req::form(u, format!("/edit/{pid}/unpublish"), &sid, &[]));
        }
        // (Often the newest: its id is the next one; often a published one:
        // feeds must drop it.)
        "delete a post" if !posts.is_empty() => {
            let public: Vec<u64> = posts.iter().copied().filter(|p| published.iter().any(|x| x.0 == *p)).collect();
            let pid = match rng.below(3) {
                0 => *posts.iter().max().expect("some"),
                1 if !public.is_empty() => public[pick(rng, public.len())],
                _ => posts[pick(rng, posts.len())],
            };
            // (The home page before and after, as a person looks: it must
            // drop a published post deleted.)
            p.then.push_back(format!("/edit/{pid}/delete#"));
            p.then.push_back("/".into());
            plain(p, net, "posts deleted", Req::get(u, "/".into(), &sid));
        }
        "delete a blog" => {
            if let Some((_, slug, _)) = mine.iter().find(|b| b.2 == 1) {
                plain(p, net, "blogs deleted", Req::form(u, format!("/dash/{slug}/delete"), &sid, &[]));
            }
        }
        "read" if !published.is_empty() => {
            let (pid, blog, slug) = &published[pick(rng, published.len())];
            p.fg = Fg::Read(*pid);
            p.fl = Some(Flight::go(net, Req::get(u, format!("/b/{blog}/{slug}"), &sid)));
        }
        "comment" if !published.is_empty() => {
            let (pid, _, _) = &published[pick(rng, published.len())];
            if let Some(pnum) = sh.pnum(*pid) {
                p.typed += 1;
                let body = format!("Nice. *cm{pnum}x{}q* [a link](https://example.com/x?a=1&b=2)", p.idx as u64 * 1_000_000 + p.typed);
                let ds = rng.below(2) == 0;
                plain(p, net, "comments", Req::form(u, format!("/comment/{pid}"), &sid, &[("body", &body), ("parent", "0"), ("after", "0")]).datastar(ds));
            }
        }
        // The rest of the site, as a person wanders it: pages, assets,
        // addresses that are wrong, methods that are not served.
        "wander" => {
            let pid = posts.first().copied().unwrap_or(1);
            let r = match rng.below(19) {
                0 => Req::get(u, "/signup".into(), &sid),
                1 => Req::get(u, "/login".into(), &None),
                2 => Req::get(u, "/recover".into(), &None),
                3 => Req::get(u, "/login?next=/dash".into(), &sid),
                4 => Req::get(u, "/s/app.css".into(), &sid),
                5 => Req::get(u, "/s/app.js?v=old".into(), &sid),
                6 => Req::get(u, "/s/nope.css".into(), &sid),
                7 => Req::get(u, format!("/handle?h=user{}", rng.below(sh.people as u64)), &None),
                8 => Req::get(u, "/handle?h=9bad".into(), &None),
                9 => Req::get(u, "/verify?t=00".into(), &None),
                10 => Req::get(u, "/u/nobody".into(), &sid),
                11 => Req::get(u, "/b/no-such-blog".into(), &sid),
                12 => Req::get(u, "/no/such/page".into(), &sid),
                13 => Req::method("HEAD", u, "/".into(), &sid),
                14 => Req::method("PUT", u, "/".into(), &sid),
                15 => Req::form(u, "/write".into(), &sid, &[]),
                16 => Req::get(u, "/edit/99999".into(), &sid),
                17 => Req::get(u, format!("/edit/{pid}/preview?title=Post%20p0."), &sid),
                _ => Req::get(u, "/dash/no-such-blog".into(), &sid),
            };
            plain(p, net, "wanders", r);
        }
        // Many writes at once (a script, an impatient person): the budget
        // must hold (the oracle counts).
        "burst" if !published.is_empty() => {
            p.burst = 40 + rng.below(50) as u32;
            stat(stats, "bursts");
        }
        // A batch that must be refused whole, on a post of one's own open tab.
        "forge a batch" if p.tabs.iter().any(|t| t.loaded) => {
            let loaded: Vec<&Tab> = p.tabs.iter().filter(|t| t.loaded).collect();
            let t = loaded[pick(rng, loaded.len())];
            let mut body = (t.since as u64).to_le_bytes().to_vec();
            body.extend_from_slice(&t.rep.to_le_bytes());
            match rng.below(6) {
                0 => body.extend([9u8, 1, 2, 3]),
                4 => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep, ctr: 0, parent: 0, side: crate::crdt::RIGHT, text: "zero" }, &mut body),
                5 => crate::crdt::encode(&crate::crdt::Op::Del { rep: 0, ctr: 1, len: 1 }, &mut body),
                1 => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep + 1000, ctr: 1, parent: 0, side: crate::crdt::RIGHT, text: "forged" }, &mut body),
                2 => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep, ctr: 4_000_000, parent: crate::crdt::key(t.rep, 3_999_999), side: crate::crdt::RIGHT, text: "orphan" }, &mut body),
                _ => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep, ctr: u32::MAX - 2, parent: 0, side: crate::crdt::RIGHT, text: "past the end" }, &mut body),
            }
            plain(p, net, "forged batches", Req::bytes(u, format!("/edit/{}/sync?me={}", t.pid, t.rep), &sid, body));
        }
        // Mail asked for one address again and again (the limit: 3 an hour).
        "mail flood" if !sh.direct => {
            // (At someone with an account: mail is sent only to those.)
            p.flood = 5 + rng.below(5) as u32;
            let who = (0..sh.people).map(|_| rng.below(sh.people as u64)).find(|i| sh.probe.int(P::UserByEmail, &[Val::Text(format!("u{i}@sim.example").as_bytes())]).is_some()).unwrap_or(0);
            // (Half by recovery, at someone with an account; half by sign-up,
            // at a fresh address: both send mail, both have the limit.)
            p.flood_to = if rng.below(2) == 0 { format!("u{who}@sim.example") } else { format!("fresh{}@sim.example", rng.below(1_000_000)) };
            stat(stats, "mail floods");
        }
        "publish a draft" if !posts.is_empty() => {
            let pid = posts[pick(rng, posts.len())];
            plain(p, net, "drafts published", Req::form(u, format!("/edit/{pid}/publish"), &sid, &[]));
        }
        // Sign-up forms filled wrong (as some are): a bad address, name or
        // username, or a username taken.
        "bad sign-up" => {
            let n = rng.below(1_000_000);
            let (name, email, handle) = match rng.below(4) {
                0 => ("Someone".to_string(), "not-an-address".to_string(), format!("z{n}")),
                1 => (String::new(), format!("z{n}@sim.example"), format!("z{n}")),
                2 => ("Someone".to_string(), format!("z{n}@sim.example"), "9-bad".to_string()),
                _ => ("Someone".to_string(), format!("z{n}@sim.example"), format!("user{}", rng.below(sh.people as u64))),
            };
            plain(p, net, "bad sign-ups", Req::form(u, "/signup".into(), &None, &[("name", &name), ("email", &email), ("handle", &handle), ("next", "/b/somewhere")]));
        }
        // A reply to a comment (its page first, sometimes, as the link does).
        "reply" if !published.is_empty() => {
            let (pid, _, _) = &published[pick(rng, published.len())];
            let cs = post_comments(&mut sh.probe, *pid);
            if let (Some(pnum), false) = (sh.pnum(*pid), cs.is_empty()) {
                let c = cs[pick(rng, cs.len())];
                if rng.below(3) == 0 {
                    let ds = rng.below(2) == 0;
                    plain(p, net, "reply pages", Req::get(u, format!("/reply/{c}"), &sid).datastar(ds));
                } else {
                    p.typed += 1;
                    let body = format!("Indeed cm{pnum}x{}q", p.idx as u64 * 1_000_000 + p.typed);
                    plain(p, net, "replies", Req::form(u, format!("/comment/{pid}"), &sid, &[("body", &body), ("parent", &c.to_string()), ("after", "0")]));
                }
            }
        }
        "more comments" if !published.is_empty() => {
            let (pid, _, _) = &published[pick(rng, published.len())];
            let cs = post_comments(&mut sh.probe, *pid);
            let after = cs.get(pick(rng, cs.len().max(1))).copied().unwrap_or(0);
            let ds = rng.below(2) == 0;
            plain(p, net, "more comments", Req::get(u, format!("/comments/{pid}?after={after}"), &sid).datastar(ds));
        }
        "like" if !published.is_empty() => {
            let (pid, _, _) = &published[pick(rng, published.len())];
            let on = if rng.below(3) == 0 { "0" } else { "1" };
            let ds = rng.below(2) == 0;
            plain(p, net, "likes", Req::form(u, format!("/like/{pid}"), &sid, &[("on", on)]).datastar(ds));
        }
        "delete a comment" => {
            let mut cs = vec![];
            sh.probe.q(P::MyComments, &[Val::Int(p.uid as i64)], |r| cs.push(r.int(0)));
            if !cs.is_empty() {
                let c = cs[pick(rng, cs.len())];
                plain(p, net, "comments deleted", Req::form(u, format!("/comment/{c}/delete"), &sid, &[]));
            }
        }
        "dashboard" => plain(p, net, "dashboards", Req::get(u, "/dash".into(), &sid)),
        // Feeds: the home page (and its later pages), a blog's, an author's.
        "feeds" => {
            let path = match rng.below(8) {
                0 => "/?page=2".to_string(),
                1 => "/?page=3".to_string(),
                2 if !mine.is_empty() => format!("/b/{}", mine[0].1),
                3 if !published.is_empty() => format!("/b/{}", published[pick(rng, published.len())].1),
                4 => format!("/u/user{}", rng.below(sh.people as u64)),
                _ => "/".to_string(),
            };
            plain(p, net, "feed pages", Req::get(u, path, &sid))
        }
        // An image for one of my posts (a PNG: its magic bytes, then noise).
        "upload an image" if !posts.is_empty() => {
            let pid = posts[pick(rng, posts.len())];
            // (PNG, JPEG, GIF or WebP: their first bytes, then noise.)
            let mut img: Vec<u8> = match rng.below(4) {
                0 => b"\x89PNG\r\n\x1a\n".to_vec(),
                1 => b"\xff\xd8\xff\xe0".to_vec(),
                2 => b"GIF89a".to_vec(),
                _ => b"RIFF\x10\x00\x00\x00WEBPVP8 ".to_vec(),
            };
            img.extend((0..16 + rng.below(3000)).map(|_| rng.next() as u8));
            p.fg = Fg::Upload(img.clone());
            p.fl = Some(Flight::go(net, Req::bytes(u, format!("/upload/{pid}"), &sid, img)));
        }
        "see an image" if !sh.images.is_empty() => {
            let keys: Vec<&String> = sh.images.keys().collect();
            let k = keys[pick(rng, keys.len())].clone();
            plain(p, net, "images seen", Req::get(u, format!("/img/{k}"), &sid));
        }
        // The editor without JavaScript: the form's text replaces the document.
        "save by form" if !posts.is_empty() => {
            let pid = posts[pick(rng, posts.len())];
            if let Some(pnum) = sh.pnum(pid) {
                // (Begun and ended by words new to the text: the server's
                // change is the smallest splice, and its tokens must not be
                // made of an old token's characters.)
                // (And begun and ended by letters that share bytes, so the
                // splice's edges fall inside characters: é è, é ũ.)
                let mut body = format!("{}Z{} Rewritten.", ["é", "è"][rng.below(2) as usize], rng.next() % 1_000_000_007);
                for _ in 0..1 + rng.below(3) {
                    p.typed += 1;
                    body.push_str(&format!(" tk{pnum}x{}q", p.idx as u64 * 1_000_000 + p.typed));
                }
                body.push_str(&format!(" Z{}.{}", rng.next() % 1_000_000_007, ["é", "ũ"][rng.below(2) as usize]));
                // (Now and then the same text again: nothing to change.)
                if let (Some(prev), true) = (p.last_form.get(&pid), rng.below(4) == 0) {
                    body = prev.clone();
                }
                p.last_form.insert(pid, body.clone());
                plain(p, net, "form saves", Req::form(u, format!("/edit/{pid}"), &sid, &[("title", &format!("Post p{pnum}.")), ("body", &body)]));
            }
        }
        // A new device: no passkey here; recovered by email (not in test mode).
        "lose the passkey" if !sh.direct && p.key.is_some() => {
            p.key = None;
            p.pending = None;
            p.sid = None;
            p.lost = true;
            stat(stats, "passkeys lost");
        }
        // Reload a tab (its unsent edits kept on the device, first).
        "reload a tab" if !p.tabs.is_empty() => {
            let t = pick(rng, p.tabs.len());
            let (pid, pnum) = (p.tabs[t].pid, p.tabs[t].pnum);
            close_tab(p, t, net, true);
            open_tab(p, pid, pnum, net);
            stat(stats, "reloads");
        }
        "close a tab" if !p.tabs.is_empty() => {
            let t = pick(rng, p.tabs.len());
            close_tab(p, t, net, true);
        }
        // The browser shut down (tabs gone, only what storage had kept).
        "restart the browser" => {
            for t in (0..p.tabs.len()).rev() {
                close_tab(p, t, net, false);
            }
            stat(stats, "browser restarts");
        }
        // Offline for a while (what is on its way is cut).
        "go offline" => {
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
        "log out" => {
            plain(p, net, "logouts", Req::form(u, "/logout".into(), &sid, &[]));
            p.sid = None;
        }
        // CURIOUS: try something that may not be allowed, on anyone's post,
        // blog or comment (the oracle judges what comes back, and any
        // change made).
        "curious" => {
            curious(p, net, sh, rng);
            stat(stats, "curious attempts");
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

fn post_comments(pr: &mut Probe, pid: u64) -> Vec<i64> {
    let mut v = vec![];
    pr.q(P::PostComments, &[Val::Int(pid as i64)], |r| v.push(r.int(0)));
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
    let r = match rng.below(17) {
        // (Things that are not what they claim: an upload that is no
        // image; a post address that is not one; mail asked for someone
        // else, again and again; a path that is not ASCII.)
        13 if !posts.is_empty() => Req::bytes(u, format!("/upload/{pid}"), &sid, if rng.below(2) == 0 { b"RIFF\x10\x00\x00\x00WAVEfmt noise".to_vec() } else { b"not an image at all".to_vec() }),
        14 => {
            let bad = ["UPPER", "draft-mine", "has space", &"x".repeat(81), "ok-address"][rng.below(5) as usize].to_string();
            let (_, bslug) = &blogs[rng.below(blogs.len() as u64) as usize];
            Req::form(u, format!("/dash/{bslug}/posts"), &sid, &[("title", "Untitled"), ("slug", &bad)])
        }
        15 => Req::form(u, "/recover".into(), &None, &[("email", &format!("u{}@sim.example", rng.below(sh.people as u64)))]),
        16 => Req::get(u, "/b/caf\u{e9}".into(), &sid),
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
        // (A batch that must be refused whole, on a post of one's own open
        // tab: not operations at all; an insert under another replica's
        // number; an insert after a character that does not exist.)
        12 if !p.tabs.is_empty() => {
            let t = &p.tabs[0];
            let mut body = (t.since as u64).to_le_bytes().to_vec();
            body.extend_from_slice(&t.rep.to_le_bytes());
            match rng.below(4) {
                0 => body.extend([9u8, 1, 2, 3]),
                3 => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep, ctr: u32::MAX - 2, parent: 0, side: crate::crdt::RIGHT, text: "past the end" }, &mut body),
                1 => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep + 1000, ctr: 1, parent: 0, side: crate::crdt::RIGHT, text: "forged" }, &mut body),
                _ => crate::crdt::encode(&crate::crdt::Op::Ins { rep: t.rep, ctr: 4_000_000, parent: crate::crdt::key(t.rep, 3_999_999), side: crate::crdt::RIGHT, text: "orphan" }, &mut body),
            }
            Req::bytes(u, format!("/edit/{}/sync?me={}", t.pid, t.rep), &sid, body)
        }
        10 if !comments.is_empty() => Req::form(u, format!("/comment/{}/delete", comments[rng.below(comments.len() as u64) as usize]), &sid, &[]),
        // (A token only for a post the writers named: others have none.)
        _ => {
            let body = if sh.pnum(pid).is_some() { format!("Curious *cm{pnum}x{}q*", u as u64 * 1_000_000 + 999_999) } else { "Curious.".to_string() };
            Req::form(u, format!("/comment/{pid}"), &sid, &[("body", &body), ("parent", "0"), ("after", "0")])
        }
    };
    p.fg = Fg::Plain("curious answered");
    p.fl = Some(Flight::go(net, r));
}

/// Signs up (no passkey yet), or in (with the one the device has, or one
/// whose registration's answer was lost).
fn sign_in(p: &mut Person, net: &mut Net, sh: &mut Shared, _rng: &mut Rng) {
    let u = p.idx;
    // (Nothing to recover without an account: sign up.)
    if p.lost && sh.probe.int(P::UserByEmail, &[Val::Text(p.email.as_bytes())]).is_none() {
        p.lost = false;
    }
    if p.key.is_none() && p.pending.is_some() {
        p.key = p.pending.take();
        p.tentative = true;
    }
    if p.key.is_none() && p.lost {
        // (A new device: a link by email adds a passkey to the account.)
        p.fg = Fg::Recover;
        p.fl = Some(Flight::go(net, Req::form(u, "/recover".into(), &None, &[("email", &p.email.clone())])));
    } else if p.key.is_none() {
        p.fg = Fg::Signup;
        let email = p.email.clone();
        let handle = format!("user{u}");
        p.fl = Some(Flight::go(net, Req::form(u, "/signup".into(), &None, &[("name", &format!("User {u}")), ("email", &email), ("handle", &handle)])));
    } else {
        p.fg = Fg::LoginOptions;
        p.fl = Some(Flight::go(net, Req::form(u, "/passkey/login/options".into(), &None, &[])));
    }
}

fn final_dash(p: &mut Person, net: &mut Net) {
    p.fg = Fg::FinalDash;
    p.fl = Some(Flight::go(net, Req::get(p.idx, "/dash".into(), &p.sid)));
}

/// The token of the latest link emailed to this address (the outbox, read
/// as an inbox), if any.
fn mail_token(pr: &mut Probe, email: &str) -> Option<String> {
    let mut body = String::new();
    pr.q(P::Mail, &[Val::Text(email.as_bytes())], |r| body = r.text(0).to_string());
    let t = body.split("t=").nth(1)?;
    let t: String = t.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
    if t.len() == 64 { Some(t) } else { None }
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
    let Some(r) = done else {
        // (No answer to a registration: the device keeps the passkey it made.)
        if let Fg::Register(_, key) = fg {
            p.pending = Some(key);
        }
        return;
    };
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
        Fg::Signup | Fg::Recover => {
            // Test mode (no mail): on to making a passkey, with the link's
            // token. Else the link comes by email (the outbox is the inbox),
            // and its page first.
            let t = if r.status == 303 {
                r.header("location").and_then(|l| l.split("t=").nth(1).map(|t| t.split('&').next().unwrap_or("").to_string()))
            } else if r.status == 200 {
                mail_token(&mut sh.probe, &p.email)
            } else {
                None
            };
            if r.status == 409 && matches!(fg, Fg::Signup) && p.pending.is_none() {
                // (An account with no passkey on this device: recover it.)
                p.lost = !sh.direct;
            }
            if let Some(t) = t {
                if r.status == 303 {
                    p.fg = Fg::RegOptions(t.clone());
                    p.fl = Some(Flight::go(net, Req::form(u, "/passkey/register/options".into(), &None, &[("t", &t)])));
                } else {
                    p.fg = Fg::Verify(t.clone());
                    p.fl = Some(Flight::go(net, Req::get(u, format!("/verify?t={t}"), &None)));
                }
            }
        }
        Fg::Verify(t) => {
            if r.status == 200 {
                p.fg = Fg::RegOptions(t.clone());
                p.fl = Some(Flight::go(net, Req::form(u, "/passkey/register/options".into(), &None, &[("t", &t)])));
            }
        }
        Fg::Upload(img) => {
            if r.status == 200 {
                if let Some(k) = r.text().strip_prefix("/img/") {
                    sh.images.insert(k.trim().to_string(), img);
                    stat(stats, "images uploaded");
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
            match r.sid().filter(|_| r.status == 200) {
                Some(s) => {
                    p.sid = Some(s);
                    p.key = Some(key);
                    p.lost = false;
                    p.uid = sh.probe.int(P::UserByEmail, &[Val::Text(p.email.as_bytes())]).unwrap_or(0) as u64;
                    stat(stats, "sign-ups");
                    if p.last == 1 {
                        final_dash(p, net);
                    }
                }
                // (A refusal: the key was not stored.)
                None => {}
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
            if r.status == 200 && r.sid().is_some() {
                p.sid = r.sid();
                p.tentative = false;
                if p.uid == 0 {
                    p.uid = sh.probe.int(P::UserByEmail, &[Val::Text(p.email.as_bytes())]).unwrap_or(0) as u64;
                }
                stat(stats, "logins");
                if p.last == 1 {
                    final_dash(p, net);
                }
            } else if r.status == 403 && p.tentative {
                // (Its registration never went through: sign up again.)
                p.key = None;
                p.tentative = false;
            }
        }
        Fg::FinalDash => {
            if r.status == 200 && r.text().contains("action=\"/logout\"") {
                p.last = 2;
            }
        }
        Fg::NewPost(pnum) => {
            if r.status == 303 {
                if let Some(pid) = r.header("location").and_then(|l| l.strip_prefix("/edit/").and_then(|x| x.parse::<u64>().ok())) {
                    sh.made(pnum, pid);
                    stat(stats, "posts");
                    if p.tabs.len() < 3 {
                        open_tab(p, pid, pnum, net);
                        // (Where pastes are on, a post is sometimes begun by
                        // pasting a novel, a few seconds in: co-authors may
                        // be there already, on the empty post.)
                        if let Some(t) = p.tabs.last_mut().filter(|_| sh.pastes && rng.below(2) == 0) {
                            t.novel = true;
                            t.type_at = net.now + 3000;
                        }
                    }
                }
            }
        }
        Fg::Read(pid) => {
            // Then follow its comments, live (the page's request, waiting).
            if r.status == 200 && rng.below(3) == 0 {
                let after = r.attr("comments", "data-signals:cafter").and_then(|a| a.parse::<u64>().ok()).unwrap_or(0);
                p.fg = Fg::Live(pid, 1 + rng.below(4) as u32);
                p.fl = Some(Flight::go(net, Req::get(u, format!("/live/{pid}?n=0&after={after}"), &p.sid).datastar(true)));
            }
        }
        Fg::Live(pid, left) => {
            let t = r.text();
            let after = t.split("\"cafter\": ").nth(1).and_then(|x| x.split(|c: char| !c.is_ascii_digit()).next()).and_then(|x| x.parse::<u64>().ok());
            if r.status == 200 && left > 1 {
                if let Some(a) = after {
                    p.fg = Fg::Live(pid, left - 1);
                    p.fl = Some(Flight::go(net, Req::get(u, format!("/live/{pid}?n={left}&after={a}"), &p.sid).datastar(true)));
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
        novel: false,
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
    let unsent: Vec<Batch> = tab.batches.iter().cloned().collect();
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
fn trace(u: usize, tab: &Tab, what: &str) {
    if std::env::var("APPSIM_TAB").ok().as_deref() == Some(&format!("{u}:{}", tab.pid)) {
        eprintln!("TAB user {u} post {} rep {} since {} loaded {} len {}: {what}", tab.pid, tab.rep, tab.since, tab.loaded, tab.doc.len16());
    }
}

fn take(tab: &mut Tab, r: &Resp, sh: &mut Shared, u: usize) -> Option<bool> {
    let b = &r.body;
    trace(u, tab, &format!("answer kind {} seq {} more {} ops {} bytes", b.first().copied().unwrap_or(9), if b.len() >= 9 { i64::from_le_bytes(b[1..9].try_into().unwrap()) } else { -1 }, b.get(9).copied().unwrap_or(9), b.len().saturating_sub(10)));
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
        sh.violation(u, "sync", format!("operations the CRDT refuses: {e:?} (post {}, since {} -> {seq}, answer kind {}, {} bytes of operations, loaded {})", tab.pid, tab.since, b[0], b.len() - at, tab.loaded));
    }
    tab.since = tab.since.max(seq);
    Some(more)
}

/// One step of a tab: its page, sending, waiting, typing, storing.
fn tab(p: &mut Person, t: usize, net: &mut Net, sh: &mut Shared, rng: &mut Rng, stats: &mut BTreeMap<&'static str, u64>, quiet: bool, paste: u64) {
    let now = net.now;
    let u = p.idx;
    let sid = p.sid.clone();
    // The page: the replica number, then the first sync with what was kept.
    if let Some(f) = &mut p.tabs[t].page {
        let Some(done) = f.poll(net) else { return };
        let tab = &mut p.tabs[t];
        tab.page = None;
        match done.as_ref().filter(|r| r.status == 200).and_then(|r| r.attr("editor", "data-rep")).and_then(|r| r.parse::<u32>().ok()) {
            Some(rep) => {
                tab.rep = rep;
                trace(u, tab, "page loaded");
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
                    // (Stored: the oracle noted what they typed, when the
                    // server stored it.)
                    tab.batches.drain(..n.min(tab.batches.len()));
                    if !tab.loaded && more == Some(false) {
                        // Loaded: the kept edits go into the document too.
                        for b in &tab.batches {
                            let _ = tab.doc.apply_batch(&b.ops, &mut ());
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
        let r0 = tab.batches.front().map_or(tab.rep, |b| b.rep);
        let mut body = (tab.since as u64).to_le_bytes().to_vec();
        body.extend_from_slice(&r0.to_le_bytes());
        let mut n = 0;
        for b in &tab.batches {
            if b.rep != r0 || body.len() + b.ops.len() > 4 << 20 {
                break;
            }
            body.extend_from_slice(&b.ops);
            n += 1;
        }
        tab.sending = n;
        let load = if tab.loaded { "" } else { "&load=1" };
        tab.sync = Some(Flight::go(net, Req::bytes(u, format!("/edit/{}/sync?me={}{load}", tab.pid, tab.rep), &sid, body)));
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
        // two halves into what reads as another post's token. And typing
        // never inside one: a token must stay whole to be traced.)
        let text = tab.doc.text();
        let toks: Vec<(usize, usize)> = token_spans(&text);
        let units: Vec<u16> = text.encode_utf16().collect();
        let (pos, del, ins, typed) = if !toks.is_empty() && rng.below(4) == 0 {
            let (a, b) = toks[rng.below(toks.len() as u64) as usize];
            sh.deleted.insert(text[a + 1..b].to_string());
            (text[..a].encode_utf16().count() as u64, text[a..b].encode_utf16().count() as u64, String::new(), None)
        } else {
            p.typed += 1;
            // (Now and then a large paste: posts grow past the size at
            // which the server keeps snapshots.)
            let big = rng.below(1000) < paste || std::mem::take(&mut tab.novel);
            // (Up to 1.2 MB: past the 1 MiB at which a snapshot is kept.)
            let fill = if big { " ".to_string() + &"lorem ipsum dolor ".repeat(3000 + rng.below(if paste >= 10 { 70_000 } else { 17_000 }) as usize) } else { ["é", "日本", "", " and", ""][rng.below(5) as usize].to_string() };
            if big {
                stat(stats, "large pastes");
            }
            let tok = format!("tk{}x{}q", tab.pnum, p.idx as u64 * 1_000_000 + p.typed);
            // A place outside every token (and not inside a character).
            let mut at = rng.below(len + 1) as usize;
            for &(a, b) in &toks {
                if at > a && at < b {
                    at = b;
                }
            }
            let at = text.char_indices().map(|(i, _)| i).chain([text.len()]).find(|&i| i >= at.min(text.len())).unwrap_or(text.len());
            (text[..at].encode_utf16().count() as u64, 0, format!(" {tok}{fill}"), Some(tok))
        };
        let r = tab.doc.edit(tab.rep, pos, del, &ins, &mut out);
        if let Err(e) = r {
            sh.violation(u, "edit", format!("a local edit the CRDT refuses: {e:?}"));
            return;
        }
        // THE EDIT DOES WHAT IT SAYS: the text is now the splice (whatever
        // operations it made, or none).
        let mut want: Vec<u16> = units[..pos as usize].to_vec();
        want.extend(ins.encode_utf16());
        want.extend_from_slice(&units[(pos + del) as usize..]);
        if tab.doc.text().encode_utf16().ne(want.iter().copied()) {
            sh.violation(u, "edit", format!("a local edit (at {pos}, {del} removed, {:?} put in) did not make the text the splice", ins));
        }
        trace(u, tab, &format!("edit at {pos}: {del} removed, {} put in", ins.len()));
        if !out.is_empty() {
            tab.batches.push_back(Batch { rep: tab.rep, ops: out, toks: typed.into_iter().collect() });
            tab.send_at = tab.send_at.min(now + 100);
            stat(stats, "edits");
        }
    }
    if now - tab.stored_at >= 1000 {
        tab.stored_at = now;
        store(p, t);
    }
}
