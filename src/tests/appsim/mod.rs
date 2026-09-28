// WHOLE-APPLICATION DETERMINISTIC SIMULATION (DESIGN.md, "Whole-app
// simulation"). The real Site (SQLite, the CRDT, sessions, comments,
// parked requests) runs in several workers in one process, on one database
// file and one set of change counters, as the worker processes do; seeded
// users use it over a simulated network; faults come from the seed:
//   - the network: latency, bytes in pieces, partial writes, clients that
//     go offline or give up;
//   - the wall clock jumping (weeks forward, minutes back) apart from the
//     monotonic one;
//   - database errors injected at Store, and workers crashing
//     mid-request (and starting again: memory gone, the database kept);
// and the oracle (oracle.rs) checks every answer as it is made. At the end
// everything is made quiet, and every open editor must show the server's
// text. One seed replays exactly: everything that varies comes from it
// (hash maps keyed by it, SQLite's query deadlines on the simulated clock).
//
// usage: build/server test appsim                  the check's budget
//        build/server test appsim SEED [SECONDS]   one seed (to replay)
//        build/server test appsim sweep FROM TO [SECONDS]
//                                                  seeds until one fails
// To look into a failure: APPSIM_TRACE=<text> prints every request or
// answer holding it (a token, say) as the oracle sees it; APPSIM_TAB=<user>:<post>
// prints that user's tabs on that post (loads, answers, edits);
// APPSIM_KEEP=1 keeps the database file.
pub mod client;
pub mod law;
pub mod net;
pub mod oracle;
pub mod users;

use crate::db::{Fault, Q, Store};
use crate::server::Server;
use crate::sim::Rng;
use crate::site::{Conf, Site};
use net::{Net, WorkerIo};
use oracle::{Checked, Probe, P};
use std::cell::RefCell;
use std::rc::Rc;
use users::Users;

pub const ORIGIN: &str = "http://sim";
pub const RP_ID: &str = "sim";

/// What the harness, the users and the oracle share.
pub struct Shared {
    pub probe: Probe,
    /// Posts as the writers made them: p<n> -> the post's id.
    pub posts: Vec<Option<u64>>,
    pub violations: Vec<String>,
    pub answers: u64,
    pub now: u64,
    pub people: usize,
    /// Faults have stopped: every answer must now be a success or a
    /// refusal, never a failure.
    pub quiet: bool,
    /// Tokens the server acknowledged storing (post id, post, token), and
    /// every token anyone deleted.
    pub acked: Vec<(u64, usize, String)>,
    pub deleted: std::collections::BTreeSet<String>,
    /// Images uploaded, by key (the oracle compares what is served).
    pub images: std::collections::BTreeMap<String, Vec<u8>>,
    /// Test mode (sign-up without email; no recovery).
    pub direct: bool,
    /// The oracle's own record of each user's writes that changed content
    /// (wall-clock times), for the write budget.
    pub budget: std::collections::BTreeMap<u64, Vec<u64>>,
    /// Changes stored, by post, replica and time (monotonic).
    pub changed: Vec<(u64, u32, u64)>,
    /// Large pastes are on in this seed.
    pub pastes: bool,
}

impl Shared {
    pub fn violation(&mut self, user: usize, what: &str, msg: String) {
        let who = if user == usize::MAX { "-".to_string() } else { format!("user {user}") };
        self.violations.push(format!("t={}ms {who} {what}: {msg}", self.now));
    }

    pub fn new_pnum(&mut self) -> usize {
        self.posts.push(None);
        self.posts.len() - 1
    }

    pub fn made(&mut self, pnum: usize, pid: u64) {
        self.posts[pnum] = Some(pid);
    }

    /// The writers' name for the post with this id now (the newest made).
    pub fn pnum(&self, pid: u64) -> Option<usize> {
        self.posts.iter().rposition(|p| *p == Some(pid))
    }
}

/// Faults injected at the database, from the seed.
struct Faults {
    rng: Rng,
    on: bool,
    /// Per 200,000 statements: errors, crashes (the seed's swarm).
    error_rate: u64,
    crash_rate: u64,
    /// Every fault injected so far (the oracle compares it before and
    /// after a request: a request no fault touched must not fail).
    injected: Rc<std::cell::Cell<u64>>,
    errors: u64,
    crashes: u64,
}

impl Faults {
    fn roll(&mut self, q: Q) -> Fault {
        if !self.on {
            return Fault::None;
        }
        // (Not on ROLLBACK: SQLite's own does not fail that way.)
        if matches!(q, Q::Rollback) {
            return Fault::None;
        }
        let r = self.rng.below(200_000);
        if r < self.error_rate {
            self.errors += 1;
            self.injected.set(self.injected.get() + 1);
            Fault::Error
        } else if r < self.error_rate + self.crash_rate {
            self.crashes += 1;
            self.injected.set(self.injected.get() + 1);
            Fault::Crash
        } else {
            Fault::None
        }
    }
}

pub struct Config {
    pub seed: u64,
    pub secs: u64,
    pub workers: usize,
    pub people: usize,
    pub swarm: Swarm,
}

/// SWARM TESTING: each seed runs its own mix. Some actions off, some
/// several times as often; fault rates from none to heavy; the clock
/// still or jumping; test mode or email; browsers that keep connections or
/// not. (One seed in four runs the plain mix, everything at its default.)
pub struct Swarm {
    pub weights: Vec<u32>,
    pub error_rate: u64,
    pub crash_rate: u64,
    pub jumps: bool,
    pub rough: bool,
    pub keep_share: u64,
    pub direct: bool,
    pub paste: u64,
}

impl Swarm {
    pub fn of(seed: u64) -> Swarm {
        let mut r = Rng(seed.wrapping_mul(0x5a17_3e11) | 1);
        let plain = seed % 4 == 0;
        let pick = |r: &mut Rng, xs: &[u64]| xs[r.below(xs.len() as u64) as usize];
        let weights = users::ACTS.iter().map(|a| if plain { a.1 } else { a.1 * pick(&mut r, &[0, 1, 1, 2, 3]) as u32 }).collect();
        Swarm {
            weights,
            error_rate: if plain { 50 } else { pick(&mut r, &[0, 25, 50, 150]) },
            crash_rate: if plain { 12 } else { pick(&mut r, &[0, 6, 12, 36]) },
            jumps: plain || r.below(3) != 0,
            rough: plain || r.below(4) != 0,
            keep_share: if plain { 7 } else { pick(&mut r, &[0, 5, 9, 10]) },
            direct: if plain { true } else { r.below(2) == 0 },
            paste: if plain { 3 } else { pick(&mut r, &[0, 3, 10]) },
        }
    }

    fn describe(&self) -> String {
        let off: Vec<&str> = users::ACTS.iter().zip(&self.weights).filter(|(_, w)| **w == 0).map(|(a, _)| a.0).collect();
        format!(
            "errors {}/200k, crashes {}/200k, clock {}, network {}, {}% keep connections, {}, pastes {}/1000{}",
            self.error_rate,
            self.crash_rate,
            if self.jumps { "jumping" } else { "still" },
            if self.rough { "rough" } else { "smooth" },
            self.keep_share * 10,
            if self.direct { "test mode" } else { "email" },
            self.paste,
            if off.is_empty() { String::new() } else { format!(", off: {}", off.join(", ")) }
        )
    }
}

pub struct Outcome {
    pub violations: Vec<String>,
    pub digest: u64,
    pub report: String,
    /// Simulated ms when the first violation was seen.
    pub first_at: Option<u64>,
}

fn worker(w: usize, path: &str, net: &Rc<RefCell<Net>>, sh: &Rc<RefCell<Shared>>, faults: &Rc<RefCell<Faults>>, changes: crate::notify::Changes) -> Server<WorkerIo, Checked> {
    let mut st = Store::open(path).expect("the simulation's database");
    st.db.busy_wait(0);
    let f = faults.clone();
    st.faults = Some(Box::new(move |q| f.borrow_mut().roll(q)));
    let direct = sh.borrow().direct;
    let mut conf = Conf::new(ORIGIN, RP_ID, direct);
    conf.wait_ms = 3000;
    let site = Site::new(st, conf, changes);
    let injected = faults.borrow().injected.clone();
    Server::new(WorkerIo { net: net.clone(), w }, Checked::new(site, sh.clone(), injected))
}

pub fn run_seed(c: &Config) -> Outcome {
    let started = std::time::Instant::now();
    crate::hash::set_keys(c.seed.wrapping_mul(0x2545_f491_4f6c_dd1d), c.seed ^ 0x9e37_79b9_7f4a_7c15);
    crate::sys::sqlite::sim_clock(Some(0));
    let dir = if std::path::Path::new("/dev/shm").is_dir() { "/dev/shm".to_string() } else { std::env::temp_dir().to_string_lossy().to_string() };
    let path = format!("{dir}/appsim-{}-{}.db", std::process::id(), c.seed);
    let clean = |p: &str| {
        for s in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{p}{s}"));
        }
    };
    clean(&path);
    drop(Store::open(&path).expect("the simulation's database"));
    let net = Rc::new(RefCell::new(Net::new(c.seed)));
    let sh = Rc::new(RefCell::new(Shared { probe: Probe::open(&path), posts: vec![], violations: vec![], answers: 0, now: 0, people: c.people, quiet: false, acked: vec![], deleted: Default::default(), images: Default::default(), direct: c.swarm.direct, budget: Default::default(), changed: vec![], pastes: c.swarm.paste > 0 }));
    let faults = Rc::new(RefCell::new(Faults { rng: Rng((c.seed ^ 0xfa01_7500) | 1), on: true, error_rate: c.swarm.error_rate, crash_rate: c.swarm.crash_rate, injected: Rc::new(std::cell::Cell::new(0)), errors: 0, crashes: 0 }));
    let changes = crate::notify::Changes::private();
    let mut ws: Vec<Server<WorkerIo, Checked>> = (0..c.workers).map(|w| worker(w, &path, &net, &sh, &faults, changes)).collect();
    let mut users = Users::new(c.people, c.seed);
    users.weights = c.swarm.weights.clone();
    users.paste = c.swarm.paste;
    net.borrow_mut().rough = c.swarm.rough;
    // Most browsers keep connections; some users' do not (Connection: close).
    {
        let mut k = Rng(c.seed.wrapping_mul(0x51) | 1);
        net.borrow_mut().keeps = (0..c.people).map(|_| k.below(10) < c.swarm.keep_share).collect();
    }
    let mut rng = Rng(c.seed.wrapping_mul(31) | 1);
    let (mut jumps, mut ticks, mut restarts) = (0u64, 0u64, 0u64);
    let mut first_at = None;
    let quiet_hook = std::panic::take_hook();
    // (Simulated crashes are quiet; any other panic is shown, and a panic
    // outside a worker ends the run: a failure, with its seed.)
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload().downcast_ref::<&str>().copied().unwrap_or("");
        if msg != "simulated crash" {
            eprintln!("PANIC {info}");
        }
    }));
    let end = c.secs * 1000;
    let mut settle_from = u64::MAX;
    let mut max_wall = 0u64;
    loop {
        let now = {
            let mut n = net.borrow_mut();
            n.now += 1 + rng.below(8);
            // THE WALL CLOCK jumps (weeks forward: sessions end; minutes back).
            if settle_from == u64::MAX && c.swarm.jumps && rng.below(6000) == 0 {
                n.skew += if rng.below(3) == 0 { -(rng.below(600_000) as i64) } else { (3_600_000 + rng.below(45 * 86_400_000)) as i64 };
                jumps += 1;
            }
            n.now
        };
        sh.borrow_mut().now = now;
        max_wall = max_wall.max(net.borrow().wall());
        crate::sys::sqlite::sim_clock(Some(now));
        users.step(&mut net.borrow_mut(), &mut sh.borrow_mut());
        // Workers in a random order each step.
        let mut order: Vec<usize> = (0..ws.len()).collect();
        for i in (1..order.len()).rev() {
            order.swap(i, rng.below(i as u64 + 1) as usize);
        }
        for w in order {
            let s = &mut ws[w];
            if let Err(e) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.turn(0))) {
                let msg = e.downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| e.downcast_ref::<String>().cloned()).unwrap_or_default();
                if msg != "simulated crash" {
                    sh.borrow_mut().violation(usize::MAX, "a worker", format!("panicked: {msg}"));
                }
                net.borrow_mut().crash_worker(w);
                ws[w] = worker(w, &path, &net, &sh, &faults, changes);
                restarts += 1;
            }
        }
        ticks += 1;
        // INVARIANTS between steps. No transaction is left open between
        // requests (it would hold every other worker's writes).
        for (w, srv) in ws.iter_mut().enumerate() {
            if srv.app.site.store().db.in_transaction() {
                sh.borrow_mut().violation(usize::MAX, "the database", format!("worker {w} left a transaction open after its requests (every other worker's writes wait for it)"));
            }
        }
        if ticks % 200 == 0 {
            let mut s = sh.borrow_mut();
            for (w, srv) in ws.iter().enumerate() {
                for (name, have, max) in srv.app.site.sizes().parts {
                    if have > max {
                        s.violation(usize::MAX, "memory", format!("worker {w}: {name} {have} over its limit {max}"));
                    }
                }
            }
            // Counts kept on posts match what they count.
            // At most 3 emails an hour to an address (the mail limit).
            let flooded = s.probe.int(P::MailFlood, &[]).unwrap_or(0);
            if flooded > 0 {
                s.violation(usize::MAX, "mail", format!("{flooded} emails sent while their address already had 3 in the hour before"));
            }
            let wrong = s.probe.int(P::BadCounts, &[]).unwrap_or(0);
            if wrong > 0 {
                s.violation(usize::MAX, "counts", format!("{wrong} posts whose like or comment count is not the number of their likes or comments"));
            }
            let min = s.probe.int(P::MinDate, &[]).unwrap_or(i64::MAX);
            if min < 1_600_000_000_000 {
                s.violation(usize::MAX, "dates", format!("a stored date before 2020 ({min} ms since 1970): time stored that is not the wall clock's"));
            }
        }
        if first_at.is_none() && !sh.borrow().violations.is_empty() {
            first_at = Some(now);
        }
        // THE END: faults off, everyone online, nothing new; then quiet.
        if now >= end && settle_from == u64::MAX {
            settle_from = now;
            faults.borrow_mut().on = false;
            net.borrow_mut().rough = false;
            // (An hour past the latest the wall clock ever read, whatever its
            // jumps back: the mail limit, 3 an hour, lets everyone try again.)
            let mut n = net.borrow_mut();
            let target = max_wall + 3_660_000;
            n.skew += target as i64 - n.wall() as i64;
            drop(n);
            sh.borrow_mut().quiet = true;
            users.settle();
        }
        if settle_from != u64::MAX && ((users.settled() && now > settle_from + 5000) || now > settle_from + 120_000) {
            break;
        }
    }
    // CONVERGENCE: every open editor of a member shows the server's text.
    let mut converged = 0;
    {
        let mut s = sh.borrow_mut();
        if !users.settled() {
            let stuck: Vec<String> = users.people.iter().filter(|p| p.last != 2).map(|p| p.describe()).collect();
            if !stuck.is_empty() {
                s.violation(usize::MAX, "the end", format!("two minutes after the faults stopped, users still could not sign in and see their dashboard: {stuck:?}"));
            } else {
                s.violation(usize::MAX, "the end", "two minutes after the faults stopped, editors still had edits to send or answers to wait for".into());
            }
        }
        let wall = net.borrow().wall();
        for p in &users.people {
            let who = s.probe.who(p.sid.as_deref().map(str::as_bytes), wall);
            for t in &p.tabs {
                if t.dead || !t.loaded || who == 0 || !s.probe.member(who, t.pid) {
                    continue;
                }
                match s.probe.doc_text(t.pid) {
                    Ok(server) => {
                        let mine = t.doc.text();
                        if mine != server {
                            let i = mine.bytes().zip(server.bytes()).take_while(|(a, b)| a == b).count();
                            let near = |t: &str| String::from_utf8_lossy(&t.as_bytes()[i.min(t.len())..(i + 40).min(t.len())]).to_string();
                            s.violation(p.idx, &format!("editor of post {}", t.pid), format!("not converged: {} vs the server's {} bytes, first differing at byte {i}: {:?} / {:?}", mine.len(), server.len(), near(&mine), near(&server)));
                        } else {
                            converged += 1;
                        }
                    }
                    Err(e) => s.violation(usize::MAX, "the end", format!("post {}: the stored document does not load: {e}", t.pid)),
                }
            }
        }
    }
    // NOTHING ACKNOWLEDGED IS LOST: every token the server said it stored
    // is in its post's text, unless someone deleted it or the post is gone.
    {
        let mut s = sh.borrow_mut();
        let acked = std::mem::take(&mut s.acked);
        let mut texts: std::collections::BTreeMap<u64, Option<String>> = Default::default();
        let mut lost = vec![];
        for (pid, pnum, tok) in &acked {
            if s.deleted.contains(tok) {
                continue;
            }
            match s.probe.post(*pid) {
                Some((_, _, ident)) if ident.contains(&format!("p{pnum}.")) => {}
                _ => continue,
            }
            let text = texts.entry(*pid).or_insert_with(|| None);
            if text.is_none() {
                *text = s.probe.doc_text(*pid).ok();
            }
            if !text.as_deref().is_some_and(|t| t.contains(tok.as_str())) {
                lost.push(format!("{tok} (post {pid})"));
            }
        }
        if !lost.is_empty() {
            s.violation(usize::MAX, "the end", format!("{} acknowledged edits lost, e.g. {:?}", lost.len(), &lost[..lost.len().min(4)]));
        }
    }
    // EVERYTHING RELEASED: browsers shut (some leaving kept connections
    // open), then past the server's idle timeout: no worker may hold a
    // connection, a parked request, a buffer, a slot or a wait.
    users.shut(&mut net.borrow_mut());
    let until = net.borrow().now + crate::limits::IDLE_TIMEOUT_MS + 5000;
    while net.borrow().now < until {
        net.borrow_mut().now += 5;
        for s in ws.iter_mut() {
            s.turn(0);
        }
    }
    {
        let mut s = sh.borrow_mut();
        for (w, srv) in ws.iter().enumerate() {
            let waits = srv.app.site.sizes().parts.iter().find(|p| p.0 == "waiting requests").map_or(0, |p| p.1);
            if srv.open_connections() != 0 || srv.parked() != 0 || srv.buffers_in_use() != (0, 0) || srv.bodies_in_use() != 0 || srv.slots_free() != crate::limits::CONN_MAX || waits != 0 {
                s.violation(usize::MAX, "the end", format!("worker {w}, every client gone: {} connections open, {} parked, buffers in use {:?}, {} bodies, {} of {} slots free, {waits} waits kept; {:?}", srv.open_connections(), srv.parked(), srv.buffers_in_use(), srv.bodies_in_use(), srv.slots_free(), crate::limits::CONN_MAX, net.borrow().describe(w)));
            }
        }
    }
    std::panic::set_hook(quiet_hook);
    crate::sys::sqlite::sim_clock(None);
    let n = net.borrow();
    let s = sh.borrow();
    let f = faults.borrow();
    let mut report = format!(
        "seed {} [{}]: {} s simulated ({} s run), {} workers, {} people: {} answers checked, {} connections; {} worker crashes, {} database errors injected, {} clock jumps; {} editors converged",
        c.seed,
        c.swarm.describe(),
        n.now / 1000,
        started.elapsed().as_secs(),
        c.workers,
        c.people,
        s.answers,
        n.conns.len(),
        restarts,
        f.errors,
        jumps,
        converged
    );
    for (k, v) in &users.stats {
        report.push_str(&format!("; {k} {v}"));
    }
    // The run in one number: what every client received, and the database.
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    for b in n.digest.to_le_bytes().iter().chain(s.violations.len().to_le_bytes().iter()) {
        h = (h ^ *b as u64).wrapping_mul(0x100_0000_01b3);
    }
    let out = Outcome { violations: s.violations.clone(), digest: h, report, first_at };
    drop((n, s, f));
    drop(ws);
    if std::env::var("APPSIM_KEEP").is_ok() {
        println!("  database kept: {path}");
    } else {
        clean(&path);
    }
    out
}

pub fn run(args: &[String]) {
    let num = |i: usize, d: u64| args.get(i).and_then(|s| s.parse().ok()).unwrap_or(d);
    let cfg = |seed: u64, secs: u64| {
        let mut r = Rng(seed.wrapping_mul(0x7f4a_7c15) | 1);
        let plain = seed % 4 == 0;
        Config { seed, secs, workers: if plain { 3 } else { 1 + r.below(4) as usize }, people: if plain { 12 } else { 6 + r.below(15) as usize }, swarm: Swarm::of(seed) }
    };
    let show = |o: &Outcome, secs: u64, seed: u64| {
        println!("{}", o.report);
        for v in o.violations.iter().take(8) {
            println!("  VIOLATION {v}");
        }
        if o.violations.len() > 8 {
            println!("  ... {} violations in all", o.violations.len());
        }
        if !o.violations.is_empty() {
            println!("  replay: build/server test appsim {seed} {secs}");
        }
    };
    match args.first().map(String::as_str) {
        Some("sweep") => {
            // Seeds until one fails (the acceptance tests).
            let (from, to, secs) = (num(1, 1), num(2, 100), num(3, 120));
            let t = std::time::Instant::now();
            for seed in from..=to {
                let o = run_seed(&cfg(seed, secs));
                if !o.violations.is_empty() {
                    show(&o, secs, seed);
                    println!("FOUND by seed {seed} at {} s simulated, after {:.0} s of running", o.first_at.unwrap_or(0) / 1000, t.elapsed().as_secs_f64());
                    std::process::exit(1);
                }
            }
            println!("NOT FOUND in seeds {from}..={to} ({:.0} s)", t.elapsed().as_secs_f64());
        }
        Some(s) if s.parse::<u64>().is_ok() => {
            let (seed, secs) = (num(0, 1), num(1, 120));
            let o = run_seed(&cfg(seed, secs));
            show(&o, secs, seed);
            println!("digest {:016x}", o.digest);
            std::process::exit(if o.violations.is_empty() { 0 } else { 1 });
        }
        _ => {
            // THE CHECK'S BUDGET: a fixed set of seeds, and a replay.
            let (seeds, secs) = (50u64, 120u64);
            let mut fails = 0;
            let t = std::time::Instant::now();
            for seed in 1..=seeds {
                let o = run_seed(&cfg(seed, secs));
                if !o.violations.is_empty() || seed == 1 {
                    show(&o, secs, seed);
                }
                if !o.violations.is_empty() {
                    fails += 1;
                }
            }
            println!("{seeds} seeds of {secs} simulated seconds ({:.0} s)", t.elapsed().as_secs_f64());
            let a = run_seed(&cfg(3, 30));
            let b = run_seed(&cfg(3, 30));
            let same = a.digest == b.digest && a.report.split(" s run").nth(1) == b.report.split(" s run").nth(1);
            println!("{} the same seed replays exactly (digest {:016x})", if same { "PASS" } else { "FAIL" }, a.digest);
            if !same {
                fails += 1;
            }
            println!("\n{fails} failure(s)");
            if fails > 0 {
                std::process::exit(1);
            }
        }
    }
}
