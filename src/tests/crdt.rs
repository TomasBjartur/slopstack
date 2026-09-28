// The CRDT (src/crdt.rs) against the model: random histories on several
// replicas, delivered in random causal orders, must end with every replica
// holding the text a naive walk of the model's tree gives (one element per
// character, children in id order), and each view kept up to date by the
// reported changes alone. Also: snapshots round-trip, bad operations are
// refused without changing anything, and the 10x performance budgets.
use crate::crdt::{self, key, Bad, Doc, Op, Sink, LEFT, RIGHT, ROOT, RUN_MAX};
use crate::sim::Rng;
use std::collections::HashMap;
use std::time::Instant;

/// The model: one element per character; the text is the in-order walk.
#[derive(Default)]
struct Naive {
    elems: HashMap<u64, (char, bool)>,
    kids: HashMap<u64, (Vec<u64>, Vec<u64>)>,
}

impl Naive {
    fn apply(&mut self, op: &Op) {
        match *op {
            Op::Ins { rep, ctr, parent, side, text } => {
                for (i, c) in text.chars().enumerate() {
                    let id = key(rep, ctr + i as u32);
                    let (p, s) = if i == 0 { (parent, side) } else { (key(rep, ctr + i as u32 - 1), RIGHT) };
                    self.elems.insert(id, (c, false));
                    let k = self.kids.entry(p).or_default();
                    let v = if s == LEFT { &mut k.0 } else { &mut k.1 };
                    let at = v.partition_point(|&x| x < id);
                    v.insert(at, id);
                }
            }
            Op::Del { rep, ctr, len } => {
                for c in ctr..ctr + len {
                    self.elems.get_mut(&key(rep, c)).expect("present").1 = true;
                }
            }
        }
    }

    fn text(&self) -> String {
        let mut out = String::new();
        // (node, expanded?)
        let mut stack: Vec<(u64, bool)> = vec![(ROOT, false)];
        while let Some((x, open)) = stack.pop() {
            if open {
                if x != ROOT {
                    let (c, dead) = self.elems[&x];
                    if !dead {
                        out.push(c);
                    }
                }
                continue;
            }
            let (l, r) = self.kids.get(&x).cloned().unwrap_or_default();
            for &c in r.iter().rev() {
                stack.push((c, false));
            }
            stack.push((x, true));
            for &c in l.iter().rev() {
                stack.push((c, false));
            }
        }
        out
    }
}

/// A view kept only from reported changes (UTF-16 units, as the browser).
struct View(Vec<u16>);

impl Sink for View {
    fn change(&mut self, pos: u64, del: u64, ins: &str) {
        let p = pos as usize;
        self.0.splice(p..p + del as usize, ins.encode_utf16());
    }
}

struct Msg {
    src: usize,
    clock: Vec<usize>,
    bytes: Vec<u8>,
}

const PIECES: &[&str] = &["a", "b", "hello ", "é", "日本", "🌊", "\n", "x🌊y", "  ", "#"];

fn random_text(rng: &mut Rng) -> String {
    let mut s = String::new();
    for _ in 0..1 + rng.below(4) {
        s.push_str(PIECES[rng.below(PIECES.len() as u64) as usize]);
    }
    s
}

/// One history: n replicas, steps edits and deliveries. Answers failures.
fn history(seed: u64, n: usize, steps: usize) -> Vec<String> {
    let mut rng = Rng(seed);
    let mut docs: Vec<Doc> = (0..n).map(|_| Doc::new()).collect();
    let mut views: Vec<View> = (0..n).map(|_| View(vec![])).collect();
    let mut got: Vec<Vec<usize>> = vec![vec![0; n]; n];
    let mut msgs: Vec<Vec<Msg>> = (0..n).map(|_| vec![]).collect();
    let mut fails = vec![];
    let deliver = |y: usize, s: usize, docs: &mut Vec<Doc>, views: &mut Vec<View>, got: &mut Vec<Vec<usize>>, msgs: &Vec<Vec<Msg>>| -> bool {
        let i = got[y][s];
        let Some(m) = msgs[s].get(i) else { return false };
        if (0..n).any(|t| t != s && m.clock[t] > got[y][t]) {
            return false;
        }
        docs[y].apply_batch(&m.bytes, &mut views[y]).expect("a good batch applies");
        got[y][s] += 1;
        true
    };
    for _ in 0..steps {
        let x = rng.below(n as u64) as usize;
        if rng.chance(0.55) {
            // A local edit.
            let len = docs[x].len16();
            let pos = rng.below(len + 1);
            let del = if len > pos && rng.chance(0.4) { 1 + rng.below((len - pos).min(12)) } else { 0 };
            let ins = if del == 0 || rng.chance(0.5) { random_text(&mut rng) } else { String::new() };
            if del == 0 && ins.is_empty() {
                continue;
            }
            let mut out = vec![];
            match docs[x].edit(x as u32 + 2, pos, del, &ins, &mut out) {
                Ok(()) => {
                    let p = pos as usize;
                    views[x].0.splice(p..p + del as usize, ins.encode_utf16());
                    let mut clock = got[x].clone();
                    clock[x] = msgs[x].len();
                    msgs[x].push(Msg { src: x, clock, bytes: out });
                    got[x][x] = msgs[x].len();
                }
                Err(Bad::Position) => {
                    if !out.is_empty() {
                        fails.push(format!("seed {seed}: a refused edit made operations"));
                    }
                }
                Err(e) => fails.push(format!("seed {seed}: edit failed {e:?}")),
            }
        } else {
            let y = rng.below(n as u64) as usize;
            let s = rng.below(n as u64) as usize;
            if s != y {
                deliver(y, s, &mut docs, &mut views, &mut got, &msgs);
            }
        }
    }
    // Deliver everything, in random order.
    loop {
        let mut any = false;
        for _ in 0..n * n * 4 {
            let y = rng.below(n as u64) as usize;
            let s = rng.below(n as u64) as usize;
            if s != y && deliver(y, s, &mut docs, &mut views, &mut got, &msgs) {
                any = true;
            }
        }
        if !any && (0..n).all(|y| (0..n).all(|s| s == y || got[y][s] == msgs[s].len())) {
            break;
        }
    }
    // The model: the set of all elements, then the set of deletions (a
    // set does not care about order).
    let mut naive = Naive::default();
    for pass in 0..2 {
        for m in msgs.iter().flatten() {
            let mut i = 0;
            while i < m.bytes.len() {
                let (op, next) = crdt::decode(&m.bytes, i).expect("decodes");
                if matches!(op, Op::Ins { .. }) == (pass == 0) {
                    naive.apply(&op);
                }
                i = next;
            }
            let _ = m.src;
        }
    }
    // Everything delivered again (a retry after a lost answer): no change.
    let t0 = docs[0].text();
    for m in msgs.iter().flatten() {
        if docs[0].apply_batch(&m.bytes, &mut ()).is_err() {
            fails.push(format!("seed {seed}: a repeated batch was refused"));
            break;
        }
    }
    if docs[0].text() != t0 {
        fails.push(format!("seed {seed}: a repeated batch changed the text"));
    }
    let want = naive.text();
    for (y, d) in docs.iter().enumerate() {
        let t = d.text();
        if t != want {
            fails.push(format!("seed {seed}: replica {y} has {t:?}, the model {want:?}"));
            break;
        }
        if views[y].0 != t.encode_utf16().collect::<Vec<u16>>() {
            fails.push(format!("seed {seed}: replica {y}'s view (from reported changes) differs"));
        }
        if !d.order_ok() {
            fails.push(format!("seed {seed}: replica {y}'s order is not its tree's"));
        }
        if d.len16() != t.encode_utf16().count() as u64 || d.chars() != t.chars().count() as u64 {
            fails.push(format!("seed {seed}: replica {y}'s lengths are off"));
        }
        let mut snap = vec![];
        d.save(&mut snap);
        match Doc::load(&snap) {
            Ok(e) => {
                let mut again = vec![];
                e.save(&mut again);
                if e.text() != t || again != snap {
                    fails.push(format!("seed {seed}: snapshot does not round-trip"));
                }
            }
            Err(e) => fails.push(format!("seed {seed}: snapshot refused {e:?}")),
        }
    }
    fails
}

pub fn run() {
    let mut fails = 0;
    let mut check = |name: &str, ok: bool| {
        println!("{} {name}", if ok { "PASS" } else { "FAIL" });
        if !ok {
            fails += 1;
        }
    };

    // Basics.
    let mut d = Doc::new();
    let mut ops = vec![];
    d.edit(2, 0, 0, "hello", &mut ops).unwrap();
    d.edit(2, 5, 0, " world", &mut ops).unwrap();
    check("typing continues one run", d.runs() == 1 && d.text() == "hello world");
    d.edit(2, 5, 6, "", &mut ops).unwrap();
    d.edit(2, 0, 0, "¡", &mut ops).unwrap();
    check("delete and insert at the start", d.text() == "¡hello");
    let mut e = Doc::new();
    let n = e.apply_batch(&ops, &mut ()).unwrap();
    check("the operations rebuild it elsewhere", e.text() == "¡hello" && n == 4);
    check("a position inside a character is refused", {
        let mut f = Doc::new();
        let mut o = vec![];
        f.edit(2, 0, 0, "🌊", &mut o).unwrap();
        let before = o.len();
        f.edit(2, 1, 0, "x", &mut o) == Err(Bad::Position) && f.edit(2, 0, 1, "", &mut o) == Err(Bad::Position) && o.len() == before && f.text() == "🌊"
    });
    check("an edit past the end is refused", d.edit(2, 99, 0, "x", &mut vec![]) == Err(Bad::Position));

    // Bad operations: refused, the document unchanged.
    let snap = |d: &Doc| {
        let mut s = vec![];
        d.save(&mut s);
        s
    };
    let before = snap(&d);
    // Repeats: an insert already here, exactly, changes nothing (the
    // model's state is a set); the same ids with anything else are refused.
    let mut dup = Doc::new();
    let mut o = vec![];
    dup.edit(2, 0, 0, "abc", &mut o).unwrap();
    dup.edit(2, 1, 0, "XY", &mut o).unwrap();
    dup.edit(3, 0, 1, "", &mut o).unwrap();
    let t = dup.text();
    check("…the exact repeat is recognized", dup.apply(&Op::Ins { rep: 2, ctr: 4, parent: key(2, 2), side: LEFT, text: "XY" }, &mut ()).is_ok());
    check("a batch applied twice: the same text", dup.apply_batch(&o, &mut ()).is_ok() && dup.apply_batch(&o, &mut ()).is_ok() && dup.text() == t);
    let before_dup = snap(&dup);
    for (name, op) in [
        ("a repeat with another character", Op::Ins { rep: 2, ctr: 1, parent: ROOT, side: RIGHT, text: "Z" }),
        ("a repeat with another parent", Op::Ins { rep: 2, ctr: 4, parent: key(2, 1), side: RIGHT, text: "XY" }),
        ("a repeat longer than the original", Op::Ins { rep: 2, ctr: 4, parent: key(2, 2), side: LEFT, text: "XYZ" }),
    ] {
        check(&format!("refused: {name}"), dup.apply(&op, &mut ()).is_err() && snap(&dup) == before_dup);
    }
    let bad: Vec<(&str, Op)> = vec![
        ("an id inside a run, with another character", Op::Ins { rep: 2, ctr: 3, parent: ROOT, side: RIGHT, text: "z" }),
        ("a counter past 2^32", Op::Ins { rep: 2, ctr: 0xFFFF_FFFF, parent: ROOT, side: RIGHT, text: "zz" }),
        ("rep 0", Op::Ins { rep: 0, ctr: 5, parent: ROOT, side: RIGHT, text: "z" }),
        ("ctr 0", Op::Ins { rep: 9, ctr: 0, parent: ROOT, side: RIGHT, text: "z" }),
        ("a missing parent", Op::Ins { rep: 9, ctr: 1, parent: key(7, 7), side: RIGHT, text: "z" }),
        ("a left child of the root", Op::Ins { rep: 9, ctr: 1, parent: ROOT, side: LEFT, text: "z" }),
        ("deleting what is not there", Op::Del { rep: 2, ctr: 1, len: 1000 }),
        ("deleting another replica's missing ids", Op::Del { rep: 8, ctr: 1, len: 1 }),
    ];
    for (name, op) in bad {
        let r = d.apply(&op, &mut ());
        check(&format!("refused: {name}"), r.is_err() && snap(&d) == before);
    }
    for (name, b) in [
        ("empty text", vec![1u8, 2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0, 0, 0]),
        ("bad UTF-8", vec![1u8, 2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 0, 0, 0, 0xFF]),
        ("side 2", vec![1u8, 2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 1, 0, 0, 0, b'a']),
        ("a length past the end", vec![1u8, 2, 0, 0, 0, 9, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 9, 0, 0, 0, b'a']),
        ("a delete of 0", vec![2u8, 2, 0, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0]),
        ("an unknown kind", vec![3u8]),
        ("truncated", vec![2u8, 2, 0]),
    ] {
        check(&format!("malformed: {name}"), d.apply_batch(&b, &mut ()) == Err(Bad::Format) && snap(&d) == before);
    }
    for (name, s) in [
        ("snapshot: not one", b"XXXX".to_vec()),
        ("snapshot: truncated", before[..before.len() - 1].to_vec()),
        ("snapshot: trailing bytes", [&before[..], b"x"].concat()),
    ] {
        check(name, Doc::load(&s).is_err());
    }
    // A doctored snapshot: two runs swapped (the order is not the tree's).
    let mut a = Doc::new();
    a.edit(2, 0, 0, "ab", &mut vec![]).unwrap();
    a.edit(3, 1, 0, "X", &mut vec![]).unwrap();
    let s = snap(&a);
    // runs: "a" (rep 2), "X" (rep 3), "b" (rep 2): swap the first two records.
    let rec = |s: &[u8], i: usize| -> std::ops::Range<usize> {
        let mut o = 8;
        for _ in 0..i {
            let nb = u32::from_le_bytes(s[o + 22..o + 26].try_into().unwrap()) as usize;
            o += 26 + nb;
        }
        let nb = u32::from_le_bytes(s[o + 22..o + 26].try_into().unwrap()) as usize;
        o..o + 26 + nb
    };
    let (r0, r1) = (rec(&s, 0), rec(&s, 1));
    let mut t = s[..8].to_vec();
    t.extend_from_slice(&s[r1.clone()]);
    t.extend_from_slice(&s[r0.clone()]);
    t.extend_from_slice(&s[r1.end..]);
    check("snapshot: a doctored order is refused", a.text() == "aXb" && Doc::load(&s).is_ok() && Doc::load(&t).is_err());

    // Concurrency: two writers at the same place do not interleave.
    let mut p = Doc::new();
    let mut base = vec![];
    p.edit(2, 0, 0, "[]", &mut base).unwrap();
    let mut q = Doc::new();
    q.apply_batch(&base, &mut ()).unwrap();
    let (mut o1, mut o2) = (vec![], vec![]);
    for (i, c) in "alpha".chars().enumerate() {
        p.edit(3, 1 + i as u64, 0, &c.to_string(), &mut o1).unwrap();
    }
    for (i, c) in "BETA".chars().enumerate() {
        q.edit(4, 1 + i as u64, 0, &c.to_string(), &mut o2).unwrap();
    }
    p.apply_batch(&o2, &mut ()).unwrap();
    q.apply_batch(&o1, &mut ()).unwrap();
    check("concurrent typing at one place: no interleaving, both agree", p.text() == q.text() && (p.text() == "[alphaBETA]" || p.text() == "[BETAalpha]"));
    check("…typed letter by letter, still one run each", p.runs() <= 4);

    // Random histories against the model.
    let mut bad = vec![];
    let t0 = Instant::now();
    let mut count = 0;
    for seed in 1..=400u64 {
        let n = 2 + (seed % 4) as usize;
        bad.extend(history(seed * 7919, n, 60 + (seed % 7) as usize * 40));
        count += 1;
    }
    for seed in 1..=10u64 {
        bad.extend(history(seed * 104_729, 3, 4000));
        count += 1;
    }
    for b in bad.iter().take(5) {
        println!("  {b}");
    }
    check(&format!("{count} random histories (2 to 5 replicas, random causal delivery): all converge on the model's text; views from changes; snapshots ({:.1} s)", t0.elapsed().as_secs_f64()), bad.is_empty());

    perf(&mut check);
    println!("{fails} failure(s)");
    if fails > 0 {
        std::process::exit(1);
    }
}

// PERFORMANCE at 10x the worst case (DESIGN.md: a 6 MB post; 60 MB here).
fn perf(check: &mut dyn FnMut(&str, bool)) {
    let para = "The river of long evenings carries small boats past old walls where people talk about books and maps. ".repeat(6) + "\n\n";
    let big = |mb: usize| para.repeat(mb * 1_000_000 / para.len());
    // A paste of the whole novel, at 10x.
    let text60 = big(60);
    let t = Instant::now();
    let mut d = Doc::new();
    let mut o = vec![];
    d.edit(2, 0, 0, &text60, &mut o).unwrap();
    let paste = t.elapsed().as_secs_f64() * 1000.0;
    check(&format!("paste 60 MB (10x a novel) in one operation: {paste:.0} ms, {} bytes of operation", o.len()), paste < 2000.0 && o.len() < text60.len() + 64);
    // Typing in the middle of it, then at the end: per key.
    for (name, at) in [("the middle", d.len16() / 2), ("the end", d.len16())] {
        let t = Instant::now();
        let mut p = at;
        let keys = 20_000;
        for i in 0..keys {
            let mut o = vec![];
            if i % 10 == 9 {
                d.edit(5, p - 1, 1, "", &mut o).unwrap();
                p -= 1;
            } else {
                d.edit(5, p, 0, "x", &mut o).unwrap();
                p += 1;
            }
        }
        let us = t.elapsed().as_secs_f64() * 1e6 / keys as f64;
        check(&format!("typing at {name} of 60 MB: {us:.1} us a key"), us < 100.0);
    }
    // Scattered edits (a heavy revision): 100k edits at random places.
    let mut rng = Rng(42);
    let t = Instant::now();
    for _ in 0..100_000 {
        let pos = rng.below(d.len16());
        let mut o = vec![];
        if rng.chance(0.5) {
            let _ = d.edit(6, pos, 1.min(d.len16() - pos), "", &mut o);
        } else {
            let _ = d.edit(6, pos, 0, "edit", &mut o);
        }
    }
    let us = t.elapsed().as_secs_f64() * 1e6 / 100_000.0;
    check(&format!("100k edits at random places in 60 MB: {us:.1} us each ({} runs)", d.runs()), us < 200.0);
    // Snapshot of all that, and loading it.
    let t = Instant::now();
    let mut s = vec![];
    d.save(&mut s);
    let save = t.elapsed().as_secs_f64() * 1000.0;
    let t = Instant::now();
    let e = Doc::load(&s).unwrap();
    let load = t.elapsed().as_secs_f64() * 1000.0;
    check(&format!("snapshot 60 MB with 200k runs: save {save:.0} ms, load {load:.0} ms ({} MB)", s.len() / 1_000_000), load < 3000.0 && e.text().len() == d.text().len());
    let t = Instant::now();
    let txt = d.text();
    check(&format!("the text of 60 MB: {:.0} ms", t.elapsed().as_secs_f64() * 1000.0), txt.len() > 50_000_000 && t.elapsed().as_secs_f64() < 1.0);
    // A novel with accents (not ASCII: a character's byte in a run is
    // found by reading the run from its start), pasted at once. Runs are
    // capped, so the first edit at a new place reads a few KB, not the
    // novel (uncapped: 6.7 ms here, 30 ms in the browser, at 6M).
    let accents = "Élodie walked past the café, naïve and sure, to the fjord’s edge. ".repeat(8) + "\n\n";
    let novel = accents.repeat(6_000_000 / accents.len());
    let mut d = Doc::new();
    d.edit(2, 0, 0, &novel, &mut vec![]).unwrap();
    let longest = d.longest_run();
    check(&format!("a pasted novel is cut into runs of at most {RUN_MAX} characters ({} runs)", d.runs()), longest <= RUN_MAX && d.text() == novel);
    // The slowest edit: deleting and typing at 200 places, one after
    // another (each the first edit there).
    // (One insert first: the text buffer grows, a copy of the novel once
    // per doubling, not a cost of the place.)
    let worst = |d: &mut Doc| {
        d.edit(5, d.len16(), 0, "!", &mut vec![]).unwrap();
        let mut rng = Rng(11);
        let mut worst = 0f64;
        for i in 0..200 {
            let p = rng.below(d.len16() - 1);
            let t = Instant::now();
            if i % 2 == 0 {
                d.edit(5, p, 1, "", &mut vec![]).unwrap();
            } else {
                d.edit(5, p, 0, "é", &mut vec![]).unwrap();
            }
            worst = worst.max(t.elapsed().as_secs_f64() * 1e6);
        }
        worst
    };
    let us = worst(&mut d);
    check(&format!("edits at 200 new places in a 6 MB novel with accents: the slowest {us:.0} us"), us < 1000.0);
    // A snapshot saved before the cap (one run of the whole novel) is cut
    // when loaded, and gives the same text.
    let mut old = b"FUG1".to_vec();
    old.extend_from_slice(&1u32.to_le_bytes());
    for v in [2u32, 1, novel.chars().count() as u32, 0, 0] {
        old.extend_from_slice(&v.to_le_bytes());
    }
    old.extend_from_slice(&[1, 0]);
    old.extend_from_slice(&(novel.len() as u32).to_le_bytes());
    old.extend_from_slice(novel.as_bytes());
    let mut e = Doc::load(&old).unwrap();
    check("a snapshot with one long run loads cut, the same text, in order", e.longest_run() <= RUN_MAX && e.text() == novel && e.order_ok());
    let us = worst(&mut e);
    check(&format!("…and editing it: the slowest edit {us:.0} us"), us < 1000.0);
    // Growth: replaying N vs 10N operations of typing (a history).
    let mut times = vec![];
    for n in [20_000u64, 200_000] {
        let mut h = Doc::new();
        let mut all = vec![];
        let mut rng = Rng(7);
        for i in 0..n {
            let pos = if i % 50 == 0 { rng.below(h.len16() + 1) } else { h.len16() };
            h.edit(2, pos, 0, "w", &mut all).unwrap();
        }
        let t = Instant::now();
        let mut r = Doc::new();
        r.apply_batch(&all, &mut ()).unwrap();
        times.push(t.elapsed().as_secs_f64());
        assert_eq!(r.text(), h.text());
    }
    let g = times[1] / times[0];
    check(&format!("replaying a history: 20k ops {:.1} ms, 200k {:.1} ms ({g:.1}x for 10x)", times[0] * 1000.0, times[1] * 1000.0), g < 25.0);
}

/// A case for the Lean model (tests/crdt_lean.sh): a random history on
/// three replicas, fully synced; prints its operations, one a line
/// ("i rep ctr prep pctr side c,c,..." or "d rep ctr len"), then "=" and
/// the text Rust made, as code points.
pub fn lean_case(seed: u64) {
    let mut rng = Rng(seed.wrapping_mul(2_654_435_761) | 1);
    let n = 3;
    let mut docs: Vec<Doc> = (0..n).map(|_| Doc::new()).collect();
    let mut log: Vec<(usize, Vec<u8>)> = vec![];
    let mut seen = vec![0usize; n];
    for _ in 0..40 + rng.below(80) {
        let x = rng.below(n as u64) as usize;
        if rng.chance(0.3) {
            // Catch up on everyone else's operations (in log order).
            for (src, b) in &log[seen[x]..] {
                if *src != x {
                    docs[x].apply_batch(b, &mut ()).expect("applies");
                }
            }
            seen[x] = log.len();
            continue;
        }
        let len = docs[x].len16();
        let pos = rng.below(len + 1);
        let del = if len > pos && rng.chance(0.4) { 1 + rng.below((len - pos).min(6)) } else { 0 };
        let ins = if del == 0 || rng.chance(0.5) { random_text(&mut rng) } else { String::new() };
        let mut out = vec![];
        if docs[x].edit(x as u32 + 2, pos, del, &ins, &mut out).is_ok() && !out.is_empty() {
            // (Own operations count as seen only if nothing came between.)
            if seen[x] == log.len() {
                seen[x] += 1;
            }
            log.push((x, out));
        }
    }
    let mut all = Doc::new();
    for (_, b) in &log {
        all.apply_batch(b, &mut ()).expect("applies");
        let mut i = 0;
        while i < b.len() {
            let (op, next) = crdt::decode(b, i).unwrap();
            match op {
                Op::Ins { rep, ctr, parent, side, text } => {
                    let cs: Vec<String> = text.chars().map(|c| (c as u32).to_string()).collect();
                    println!("i {rep} {ctr} {} {} {side} {}", parent >> 32, parent as u32, cs.join(","));
                }
                Op::Del { rep, ctr, len } => println!("d {rep} {ctr} {len}"),
            }
            i = next;
        }
    }
    let t: Vec<String> = all.text().chars().map(|c| (c as u32).to_string()).collect();
    println!("= {}", t.join(" "));
}
