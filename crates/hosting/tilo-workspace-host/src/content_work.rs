//! A bound on the content work one member may have running on a host at
//! once.
//!
//! Reading content may touch the disk, so the host does it on the blocking
//! pool: one task per piece a member sends it, and one per content request
//! it answers. Both are started per frame, and a member can send frames
//! faster than any disk answers them. Without a bound, one member's flood
//! takes the pool every other member's pieces need: their content waits
//! behind it, and their requests queue behind tasks that are only waiting
//! for the same disk. Each member may hold [`PER_MEMBER`] of these tasks;
//! what it sends past them is dropped like a piece that did not fit a
//! connection's queue, and its device asks again.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tilo_lock::MutexRecover as _;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Content reads one member may have running on a workspace's host at once.
pub const PER_MEMBER: usize = 8;

/// The content work running per member.
#[derive(Default)]
pub struct ContentWork {
    places: Mutex<HashMap<[u8; 32], Arc<Semaphore>>>,
}

impl ContentWork {
    /// Take one of `member`'s places for content work, or `None` when all
    /// of them are at work.
    pub fn take_place(self: &Arc<Self>, member: &[u8; 32]) -> Option<OwnedSemaphorePermit> {
        let places = {
            let mut places = self.places.lock_recover();
            Arc::clone(
                places
                    .entry(*member)
                    .or_insert_with(|| Arc::new(Semaphore::new(PER_MEMBER))),
            )
        };
        places.try_acquire_owned().ok()
    }
}
