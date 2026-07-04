//! Heuristic prefetching hooks.
//!
//! These are intentionally light stubs: the read path already hydrates on
//! demand, and aggressive prefetch is a follow-up. They exist so the FUSE layer
//! has a place to signal access patterns (open / readdir) that a future policy
//! can turn into background `Hydrator` fetches.

use std::sync::{Arc, Mutex};

use crate::hydration::Hydrator;
use crate::namespace::Namespace;

pub struct Heuristics {
    #[allow(dead_code)]
    hydrator: Arc<Hydrator>,
    #[allow(dead_code)]
    namespace: Arc<Mutex<Namespace>>,
}

impl Heuristics {
    pub fn new(hydrator: Arc<Hydrator>, namespace: Arc<Mutex<Namespace>>) -> Self {
        Self { hydrator, namespace }
    }

    /// Called when a file inode is opened.
    pub fn on_open(&self, _ino: u64) {}

    /// Called when a directory is listed.
    pub fn on_readdir(&self, _ino: u64) {}
}
