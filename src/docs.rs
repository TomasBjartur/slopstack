// Documents on the server: each post's collaborative text (src/crdt.rs),
// kept in memory while in use, stored as batches of operations (doc_ops)
// and snapshots (doc_snap).
//
// - The database is the truth; a cached document catches up with it
//   (OpsSince) before every use, so worker processes that share the
//   database agree.
// - A client's batch is applied, inside the write transaction (BEGIN
//   IMMEDIATE: one writer at a time), to the document caught up to the
//   last stored batch; only a batch that applies whole is stored. So the
//   stored order is one every replica can apply in (each batch's
//   dependencies are stored before it), and a repeat changes nothing.
// - Inserts in a batch must use the replica number the server gave this
//   user for this post (doc_rep): nobody can make elements with another
//   writer's ids.
// - Limits: a batch at most BATCH_MAX bytes; a post's stored operations at
//   most LOG_MAX bytes (10x a novel's worth of history). A snapshot is
//   written when the operations since the last one outweigh it.
use crate::crdt::{self, Doc, Op};
use crate::db::{No, Q, Store};
use crate::sys::sqlite::Val;
use std::collections::HashMap;

pub const BATCH_MAX: usize = 16 << 20;
pub const LOG_MAX: i64 = 1 << 30;
/// Documents kept in memory (bytes, roughly).
const CACHE_MAX: usize = 768 << 20;
/// A sync answer: at most this much of operations (more: asked again).
pub const REPLY_MAX: usize = 8 << 20;

struct Loaded {
    doc: Doc,
    /// The last stored batch applied.
    seq: i64,
    used: u64,
}

pub struct Docs {
    map: HashMap<u64, Loaded>,
    tick: u64,
}

impl Default for Docs {
    fn default() -> Self {
        Docs::new()
    }
}

impl Docs {
    pub fn new() -> Docs {
        Docs { map: HashMap::new(), tick: 0 }
    }

    pub fn forget(&mut self, post: u64) {
        self.map.remove(&post);
    }

    /// The post's document, caught up with the database.
    pub fn get(&mut self, st: &mut Store, post: u64) -> Result<&mut Doc, No> {
        self.tick += 1;
        if !self.map.contains_key(&post) {
            let mut snap: Option<(i64, Vec<u8>)> = None;
            st.q(Q::SnapGet, &[Val::Int(post as i64)], |r| snap = Some((r.int(0), r.bytes(1).to_vec())))?;
            let (doc, seq) = match snap {
                Some((upto, data)) => (Doc::load(&data).map_err(|_| No::Error)?, upto),
                None => (Doc::new(), 0),
            };
            self.evict();
            self.map.insert(post, Loaded { doc, seq, used: 0 });
        }
        let l = self.map.get_mut(&post).expect("just put");
        l.used = self.tick;
        loop {
            let mut rows: Vec<(i64, Vec<u8>)> = vec![];
            st.q(Q::OpsSince, &[Val::Int(post as i64), Val::Int(l.seq), Val::Int(1000)], |r| rows.push((r.int(0), r.bytes(1).to_vec())))?;
            let n = rows.len();
            for (seq, data) in rows {
                // (Stored batches applied when stored: a failure here means
                // the database was changed by hand.)
                l.doc.apply_batch(&data, &mut ()).map_err(|_| No::Error)?;
                l.seq = seq;
            }
            if n < 1000 {
                break;
            }
        }
        Ok(&mut self.map.get_mut(&post).expect("loaded").doc)
    }

    pub fn seq(&self, post: u64) -> i64 {
        self.map.get(&post).map_or(0, |l| l.seq)
    }

    fn evict(&mut self) {
        let mut total: usize = self.map.values().map(|l| l.doc.memory()).sum();
        while total > CACHE_MAX {
            let Some((&k, _)) = self.map.iter().min_by_key(|(_, l)| l.used) else { break };
            total -= self.map.remove(&k).map_or(0, |l| l.doc.memory());
        }
    }

    /// Stores a client's batch (inside the caller's write transaction):
    /// checked for the replica, applied, stored, snapshotted if due.
    /// Answers the batch's seq (or the current one for an empty batch or
    /// a repeat).
    pub fn store(&mut self, st: &mut Store, post: u64, rep: u32, batch: &[u8]) -> Result<i64, No> {
        if batch.len() > BATCH_MAX {
            return Err(No::Bad);
        }
        // Inserts only with the writer's own replica number.
        let mut i = 0;
        while i < batch.len() {
            let (op, next) = crdt::decode(batch, i).map_err(|_| No::Bad)?;
            if let Op::Ins { rep: r, .. } = op {
                if r != rep {
                    return Err(No::Bad);
                }
            }
            i = next;
        }
        let doc = self.get(st, post)?;
        let changed = match doc.apply_batch(batch, &mut ()) {
            Ok(n) => n,
            Err(_) => {
                // Partly applied: this copy is no longer the database's.
                self.forget(post);
                return Err(No::Bad);
            }
        };
        if changed == 0 {
            // Empty, or all repeats (a retry after a lost answer).
            return Ok(self.seq(post));
        }
        let (bytes, _) = self.log_size(st, post)?;
        if bytes + batch.len() as i64 > LOG_MAX {
            self.forget(post);
            return Err(No::Conflict);
        }
        st.run(Q::OpsAdd, &[Val::Int(post as i64), Val::Blob(batch)])?;
        let seq = st.db.last_rowid();
        let l = self.map.get_mut(&post).expect("loaded");
        l.seq = seq;
        self.snapshot_if_due(st, post)?;
        Ok(seq)
    }

    /// Bytes and count of operations stored since the snapshot.
    fn log_size(&mut self, st: &mut Store, post: u64) -> Result<(i64, i64), No> {
        let mut upto = 0;
        st.q(Q::SnapUpto, &[Val::Int(post as i64)], |r| upto = r.int(0))?;
        let mut out = (0, 0);
        st.q(Q::OpsCount, &[Val::Int(post as i64), Val::Int(upto)], |r| out = (r.int(0), r.int(1)))?;
        Ok(out)
    }

    fn snapshot_if_due(&mut self, st: &mut Store, post: u64) -> Result<(), No> {
        let mut snap = (0i64, 0i64);
        st.q(Q::SnapUpto, &[Val::Int(post as i64)], |r| snap = (r.int(0), r.int(1)))?;
        let (since, _) = {
            let mut out = (0, 0);
            st.q(Q::OpsCount, &[Val::Int(post as i64), Val::Int(snap.0)], |r| out = (r.int(0), r.int(1)))?;
            out
        };
        if since < (1 << 20).max(snap.1) {
            return Ok(());
        }
        let l = self.map.get(&post).expect("loaded");
        let mut data = Vec::with_capacity(l.doc.memory());
        l.doc.save(&mut data);
        st.run(Q::SnapPut, &[Val::Int(post as i64), Val::Int(l.seq), Val::Blob(&data), Val::Int(data.len() as i64)])?;
        Ok(())
    }

    /// A sync answer for a client that has seen up to since: a snapshot
    /// (if since is before it, and it helps) and the stored batches after,
    /// up to REPLY_MAX bytes. Format: kind (0: batches only, 1: snapshot
    /// first), the last seq included (u64), more (1: ask again at once),
    /// then for kind 1 the snapshot's length (u32) and bytes, then the
    /// batches' operations one after another.
    pub fn reply(&mut self, st: &mut Store, post: u64, since: i64, out: &mut Vec<u8>) -> Result<(), No> {
        self.get(st, post)?;
        let mut snap: Option<(i64, Vec<u8>)> = None;
        if since == 0 {
            st.q(Q::SnapGet, &[Val::Int(post as i64)], |r| snap = Some((r.int(0), r.bytes(1).to_vec())))?;
        }
        let mut from = since;
        out.push(snap.is_some() as u8);
        let seq_at = out.len();
        out.extend_from_slice(&[0u8; 9]);
        if let Some((upto, data)) = &snap {
            out.extend_from_slice(&(data.len() as u32).to_le_bytes());
            out.extend_from_slice(data);
            from = *upto;
        }
        let mut last = from;
        let mut more = false;
        let start = out.len();
        loop {
            let mut got = 0;
            st.q(Q::OpsSince, &[Val::Int(post as i64), Val::Int(last), Val::Int(256)], |r| {
                if out.len() - start < REPLY_MAX {
                    out.extend_from_slice(r.bytes(1));
                    last = r.int(0);
                    got += 1;
                } else {
                    more = true;
                }
            })?;
            if got < 256 || more {
                break;
            }
        }
        out[seq_at..seq_at + 8].copy_from_slice(&(last as u64).to_le_bytes());
        out[seq_at + 8] = more as u8;
        Ok(())
    }

    /// Replaces the document's text with text (the form without
    /// JavaScript): the changed middle, as the server's replica.
    pub fn set_text(&mut self, st: &mut Store, post: u64, text: &str) -> Result<(), No> {
        let doc = self.get(st, post)?;
        let old = doc.text();
        let (a, b) = (old.as_bytes(), text.as_bytes());
        let mut p = a.iter().zip(b).take_while(|(x, y)| x == y).count();
        while !old.is_char_boundary(p) || !text.is_char_boundary(p) {
            p -= 1;
        }
        let mut s = a[p..].iter().rev().zip(b[p..].iter().rev()).take_while(|(x, y)| x == y).count();
        while !old.is_char_boundary(a.len() - s) || !text.is_char_boundary(b.len() - s) {
            s -= 1;
        }
        if p == a.len() && p == b.len() {
            return Ok(());
        }
        let u16 = |t: &str| t.encode_utf16().count() as u64;
        let pos = u16(&old[..p]);
        let del = u16(&old[p..a.len() - s]);
        let mut batch = vec![];
        doc.edit(crdt::SERVER_REP, pos, del, &text[p..b.len() - s], &mut batch).map_err(|_| No::Error)?;
        st.run(Q::OpsAdd, &[Val::Int(post as i64), Val::Blob(&batch)])?;
        let seq = st.db.last_rowid();
        self.map.get_mut(&post).expect("loaded").seq = seq;
        self.snapshot_if_due(st, post)
    }
}
