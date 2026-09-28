// Hash maps for the application's caches, whose order depends only on two
// keys set once per process: from the OS in production (so a client
// cannot pick keys that collide), from the seed in the simulator (so a
// run replays exactly: std's RandomState differs every time, and cache
// eviction follows a map's order).
use std::collections::HashMap;
use std::hash::BuildHasher;
use std::sync::atomic::{AtomicU64, Ordering};

static K0: AtomicU64 = AtomicU64::new(0x736f_6d65_7073_6575);
static K1: AtomicU64 = AtomicU64::new(0x646f_7261_6e64_6f6d);

/// Sets the keys (before any map is made).
pub fn set_keys(k0: u64, k1: u64) {
    K0.store(k0, Ordering::Relaxed);
    K1.store(k1, Ordering::Relaxed);
}

#[derive(Clone, Copy, Default)]
pub struct Keyed;

impl BuildHasher for Keyed {
    #[allow(deprecated)]
    type Hasher = std::hash::SipHasher;

    #[allow(deprecated)]
    fn build_hasher(&self) -> std::hash::SipHasher {
        std::hash::SipHasher::new_with_keys(K0.load(Ordering::Relaxed), K1.load(Ordering::Relaxed))
    }
}

pub type Map<K, V> = HashMap<K, V, Keyed>;

pub fn map<K, V>() -> Map<K, V> {
    HashMap::with_hasher(Keyed)
}
