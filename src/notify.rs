// What changed, between worker processes: counters in memory shared by
// all of them (made before they are forked), one per post and kind, bumped
// after a change is committed. A request waiting for changes (an editor's
// long poll: src/site.rs) notes a counter before reading the database,
// and is answered when the counter moves. Posts share counters (a post's
// is its id modulo SLOTS): another post's change only wakes a waiter to
// find nothing and wait again.
use std::sync::atomic::{AtomicU32, Ordering};

const SLOTS: usize = 1 << 14;

#[derive(Clone, Copy)]
pub enum Kind {
    /// A post's document (the editor's operations).
    Doc = 0,
    /// A post's comments.
    Comments = 1,
}

#[derive(Clone, Copy)]
pub struct Changes {
    c: &'static [AtomicU32],
}

impl Changes {
    /// Shared with worker processes forked after this.
    pub fn shared() -> Changes {
        let c = crate::sys::linux::shared_counters(2 * SLOTS).unwrap_or_else(|e| panic!("mmap: errno {e}"));
        Changes { c }
    }

    /// For one process (the simulator: its workers share these). Made once
    /// per thread and zeroed for each run.
    pub fn private() -> Changes {
        thread_local! {
            static MINE: &'static [AtomicU32] = Box::leak((0..2 * SLOTS).map(|_| AtomicU32::new(0)).collect());
        }
        let c = MINE.with(|m| *m);
        for x in c {
            x.store(0, Ordering::SeqCst);
        }
        Changes { c }
    }

    fn at(&self, k: Kind, post: u64) -> &AtomicU32 {
        &self.c[k as usize * SLOTS + (post % SLOTS as u64) as usize]
    }

    /// (Before reading the database: a change committed after this read
    /// moves it.)
    pub fn get(&self, k: Kind, post: u64) -> u32 {
        self.at(k, post).load(Ordering::SeqCst)
    }

    /// (After the change is committed.)
    pub fn bump(&self, k: Kind, post: u64) {
        self.at(k, post).fetch_add(1, Ordering::SeqCst);
    }
}
