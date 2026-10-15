//! Where the broker's services find the native owner, never opening it on
//! the thread that builds them.
//!
//! The session's broker is built on the compositor's loop. Opening the
//! native owner takes its writer's lease, syncs the state file's directory
//! chain and publishes the reopened generation: seconds on a loaded disk.
//! So a service only starts the open (on a thread of its own) when it is
//! built. A synchronous question (health, a snapshot, a read) answers from
//! an owner already open and says it is still opening otherwise; a commit,
//! which is a future its caller already lets wait for the disk, waits for
//! the open inside that future. Neither runs against a guessed owner.
use crate::shell::{
    native_workspace_owner::{self, NativeWorkspaceOwner},
    network::{NativeOwnerStatus, ShellNetwork},
    state::WorkspaceTreeHandle,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// What a synchronous question hears while the owner opens. It is true and
/// it passes: the open finishes on its own, and the next attempt finds it.
pub(crate) const OPENING: &str = "the native owner is still opening; try again in a moment";

/// A failed open of a network-less owner is tried again no sooner than
/// this: a second writer of the state file can hold it for long.
const REOPEN_AFTER: Duration = Duration::from_secs(1);

#[derive(Clone)]
pub(super) enum OwnerSource {
    /// The session network's owner: the one enrollment and ingress use.
    Network(Arc<ShellNetwork>),
    /// Without a network, the tree's own writer.
    Local(Arc<LocalOwner>),
}

pub(super) struct LocalOwner {
    tree: WorkspaceTreeHandle,
    open: Arc<Mutex<LocalOpen>>,
}

#[derive(Default)]
struct LocalOpen {
    /// Retained: the writer holds its owner only weakly.
    owner: Option<NativeWorkspaceOwner>,
    running: bool,
    failed_at: Option<Instant>,
}

impl OwnerSource {
    /// The source for `tree`, through `network` when the session has one.
    /// Starts opening the owner off this thread, so it is ready by the time
    /// the first request asks.
    pub(super) fn new(tree: &WorkspaceTreeHandle, network: Option<&Arc<ShellNetwork>>) -> Self {
        let source = match network {
            Some(network) => Self::Network(Arc::clone(network)),
            None => Self::Local(Arc::new(LocalOwner {
                tree: tree.clone(),
                open: Arc::default(),
            })),
        };
        drop(source.status());
        source
    }

    /// Where the owner stands, never waiting for the state file.
    pub(super) fn status(&self) -> NativeOwnerStatus {
        match self {
            Self::Network(network) => network.native_owner_status(),
            Self::Local(local) => local.status(),
        }
    }

    /// The owner if it is open; [`OPENING`] while it opens, `unavailable`
    /// when there is none. Never waits.
    pub(super) fn opened(&self, unavailable: &str) -> Result<NativeWorkspaceOwner, String> {
        match self.status() {
            NativeOwnerStatus::Open(owner) => Ok(owner),
            NativeOwnerStatus::Opening => Err(OPENING.into()),
            NativeOwnerStatus::Unavailable => Err(unavailable.into()),
        }
    }

    /// The owner, opened here if it is not yet: for a commit's future or a
    /// worker thread, which may wait for the disk. An open in progress
    /// elsewhere is waited for, never repeated.
    pub(super) fn resolve(&self) -> Result<NativeWorkspaceOwner, String> {
        match self {
            Self::Network(network) => network.native_owner(),
            Self::Local(local) => local.resolve(),
        }
    }
}

impl LocalOwner {
    fn status(&self) -> NativeOwnerStatus {
        let Ok(mut open) = self.open.lock() else {
            return NativeOwnerStatus::Unavailable;
        };
        if let Some(owner) = open.owner.as_ref() {
            return NativeOwnerStatus::Open(owner.clone());
        }
        if let Some(owner) = native_workspace_owner::attached(&self.tree) {
            open.owner = Some(owner.clone());
            return NativeOwnerStatus::Open(owner);
        }
        if open.running {
            return NativeOwnerStatus::Opening;
        }
        if self.tree.read().persist_path.is_none()
            || open
                .failed_at
                .is_some_and(|failed| failed.elapsed() < REOPEN_AFTER)
        {
            return NativeOwnerStatus::Unavailable;
        }
        open.running = true;
        drop(open);
        let tree = self.tree.clone();
        let finished = Arc::clone(&self.open);
        let spawned = std::thread::Builder::new()
            .name("native-owner-open".into())
            .spawn(move || {
                let opened = native_workspace_owner::open(tree);
                let Ok(mut open) = finished.lock() else {
                    return;
                };
                open.running = false;
                match opened {
                    Ok(owner) => {
                        open.owner.get_or_insert(owner);
                        open.failed_at = None;
                    }
                    Err(error) => {
                        open.failed_at = Some(Instant::now());
                        tracing::warn!(%error, "native durable owner unavailable");
                    }
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "cannot start opening the native owner");
            if let Ok(mut open) = self.open.lock() {
                open.running = false;
                open.failed_at = Some(Instant::now());
            }
            return NativeOwnerStatus::Unavailable;
        }
        NativeOwnerStatus::Opening
    }

    fn resolve(&self) -> Result<NativeWorkspaceOwner, String> {
        if let NativeOwnerStatus::Open(owner) = self.status() {
            return Ok(owner);
        }
        // The writer's own opening lock orders this after an open already
        // in progress, which this then finds attached.
        let owner = native_workspace_owner::open(self.tree.clone())?;
        if let Ok(mut open) = self.open.lock() {
            open.owner.get_or_insert_with(|| owner.clone());
        }
        Ok(owner)
    }
}
