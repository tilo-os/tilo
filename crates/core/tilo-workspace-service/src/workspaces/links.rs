//! Where a holder's links to a workspace's other holders stand.
//!
//! Every holder of a workspace links to the others over the device link
//! ([`tilo_peer_state::direct::DirectSync`]), and says in the workspace's
//! state how far its copy has got with each of them ([`Links`]): reaching it,
//! taking in what it holds and this device lacks, current with it, or not
//! reached since some time. Every device reads from those rows, against how
//! many copies the workspace wants, which holder is short of what.

use std::collections::BTreeMap;
use std::sync::Arc;

use tilo_lock::MutexRecover as _;

/// Where a device's link to another holder of a workspace stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    /// Not reached since this device started: what it lacks of the other
    /// is not known yet.
    Reaching,
    /// Reached, and taking in what the other holds and this device lacks.
    CatchingUp,
    /// Holding everything the other holds.
    Current,
    /// Not reached since `since_ns` (ns since the epoch).
    Unreachable { since_ns: u64 },
}

/// Where a device's links to its workspace's other holders stand.
#[derive(Clone, Default)]
pub(super) struct Links(Arc<LinksInner>);

#[derive(Default)]
struct LinksInner {
    links: std::sync::Mutex<BTreeMap<[u8; 32], Link>>,
    /// How many blobs of the content the workspace's files name this device
    /// lacks.
    content_missing: std::sync::atomic::AtomicU32,
    changed: tokio::sync::Notify,
}

impl Links {
    /// The link to `device` stands at `link`. A holder unreachable before
    /// stays unreachable since it first was.
    pub(super) fn set(&self, device: [u8; 32], link: Link) {
        let mut links = self.0.links.lock_recover();
        let link = match (links.get(&device), link) {
            (Some(Link::Unreachable { since_ns }), Link::Unreachable { .. }) => Link::Unreachable {
                since_ns: *since_ns,
            },
            _ => link,
        };
        if links.insert(device, link) != Some(link) {
            drop(links);
            self.0.changed.notify_one();
        }
    }

    /// No link to `device` is kept any more.
    pub(super) fn forget(&self, device: &[u8; 32]) {
        if self.0.links.lock_recover().remove(device).is_some() {
            self.0.changed.notify_one();
        }
    }

    /// Where each link stands now.
    pub(super) fn now(&self) -> BTreeMap<[u8; 32], Link> {
        self.0.links.lock_recover().clone()
    }

    /// Wait until a link changes, or say it changed at once.
    pub(super) async fn changed(&self) {
        self.0.changed.notified().await;
    }

    /// This device lacks `blobs` of the content the workspace names.
    pub(crate) fn lacks_content(&self, blobs: u32) {
        use std::sync::atomic::Ordering;
        if self.0.content_missing.swap(blobs, Ordering::SeqCst) != blobs {
            self.0.changed.notify_one();
        }
    }

    /// How many blobs of the content the workspace names this device lacks.
    pub(super) fn content_missing(&self) -> u32 {
        self.0
            .content_missing
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Have the next [`Self::changed`] return at once.
    pub(super) fn touch(&self) {
        self.0.changed.notify_one();
    }
}
