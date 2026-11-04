//! A member device's side of a workspace store.
//!
//! The device's replica ([`DirectSync`]) and its owner
//! ([`NativeWorkspaceOwner`]) hold the member's own history; a store is the
//! copy another device of the workspace keeps for it while it is away.
//! This crate joins them:
//!
//! * it keeps what the device publishes at the store
//!   ([`StoreSession::keep`], [`StoreSession::keep_workspace`]), which the store
//!   retains sealed and can never read;
//! * it catches the replica up from the store on start
//!   ([`StoreSession::catch_up`], [`StoreSession::catch_up_workspace`]): it asks
//!   from the replica's own cursors ([`cursors`], [`workspace_cursors`]), and
//!   admits every replayed frame as its author's retained history, the same
//!   admission a live frame from that device takes.
//!
//! Membership is not decided here: the store admits the connecting device
//! by the [`Access`](tilo_store_node::Access) its embedder gave it, and the
//! replica refuses a frame the workspace's roster does not back.
//!
//! The store device's side is here too: [`MembershipAccess`] is the engine's
//! membership answering for a store, and [`StoreService`] is the service that
//! keeps what it is given and serves it, with the appetite its device's mesh
//! policy gives it ([`appetite`]).

#![deny(rust_2018_idioms)]

pub mod access;
pub mod service;

pub use access::{MembershipAccess, held_workspace};
pub use service::{StoreService, WORK_BYTES, appetite};
pub use tilo_store_node::ALPN;

use iroh::EndpointAddr;
use neighbour_wire::NeighbourMsg;
use tilo_desktop_model::WorkspaceId;
use tilo_peer_state::direct::DirectSync;
use tilo_peer_state::owner::NativeWorkspaceOwner;
use tilo_peer_state::{StatePeer, TransportOrigin};
use tilo_store_node::{Cursor, Lineage, Link, LinkError, advance};
use tilo_user_identity::{DeviceCert, RevocationEntry};

/// Why a member could not keep or catch up its history at a store.
#[derive(Debug, thiserror::Error)]
pub enum StoreLinkError {
    /// The store could not be reached, refused the device or the request,
    /// or the session broke.
    #[error("store: {0}")]
    Store(#[from] LinkError),
    /// The replica refused a replayed frame.
    #[error("replica: {0}")]
    Replica(String),
    /// The store replayed something that is not a document frame.
    #[error("the store replayed a message that is not a document frame")]
    NotAFrame,
}

/// One device's session with its workspace's store.
pub struct StoreSession {
    link: Link,
    workspace: u64,
}

impl std::fmt::Debug for StoreSession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StoreSession")
            .field("workspace", &self.workspace)
            .finish_non_exhaustive()
    }
}

impl StoreSession {
    /// Dial `store` from this device's `endpoint`, introduce it with `cert`,
    /// and hold `workspace` there.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the store cannot be reached or does
    /// not admit this device.
    pub async fn join(
        endpoint: &iroh::Endpoint,
        store: impl Into<EndpointAddr>,
        cert: DeviceCert,
        revocations: Vec<RevocationEntry>,
        workspace: u64,
    ) -> Result<Self, StoreLinkError> {
        let link = Link::connect(endpoint, store, cert, revocations).await?;
        Ok(Self { link, workspace })
    }

    /// The workspace this session keeps at the store.
    #[must_use]
    pub const fn workspace(&self) -> u64 {
        self.workspace
    }

    /// Keep `message` at the store. Returns whether it was a sealed document
    /// frame it keeps; presence, surfaces and other live traffic is not
    /// history, and neither is a cleartext frame a store can never hold.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke or the store
    /// refused.
    pub async fn keep(&mut self, message: &NeighbourMsg) -> Result<bool, StoreLinkError> {
        if !sealed(message) {
            return Ok(false);
        }
        self.link
            .push(self.workspace, std::slice::from_ref(message))
            .await?;
        Ok(true)
    }

    /// Keep everything `owner` has published to `workspace` that it may send
    /// now, in publication order. Returns how many frames the store took.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke or the store
    /// refused, and [`StoreLinkError::Replica`] when the workspace is not
    /// held here.
    pub async fn keep_workspace(
        &mut self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        now: std::time::SystemTime,
    ) -> Result<usize, StoreLinkError> {
        let outgoing: Vec<NeighbourMsg> = owner
            .with_outgoing(workspace, now, |_, outgoing| {
                outgoing
                    .entries()
                    .map(|entry| {
                        postcard::from_bytes(entry.bytes()).map_err(|error| error.to_string())
                    })
                    .collect()
            })
            .map_err(StoreLinkError::Replica)?;
        let mut kept = 0usize;
        for message in &outgoing {
            if self.keep(message).await? {
                kept = kept.saturating_add(1);
            }
        }
        Ok(kept)
    }

    /// Admit every frame the workspace lacks, from the store, until it holds
    /// what the store has. Returns how many frames were replayed.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke, and
    /// [`StoreLinkError::Replica`] when the owner refuses a frame: the
    /// catch-up stops there, and the caller's cursors still stand where
    /// they did, so the next call asks for the refused frame again once
    /// what it needs has arrived.
    pub async fn catch_up_workspace(
        &mut self,
        owner: &NativeWorkspaceOwner,
        workspace: WorkspaceId,
        now: std::time::SystemTime,
    ) -> Result<usize, StoreLinkError> {
        let mut standing = workspace_cursors(owner, workspace).map_err(StoreLinkError::Replica)?;
        let mut replayed = 0;
        loop {
            let replay = self.link.want(self.workspace, &standing).await?;
            if replay.frames.is_empty() {
                return Ok(replayed);
            }
            for message in &replay.frames {
                let author = author_of(message).ok_or(StoreLinkError::NotAFrame)?;
                owner
                    .receive(
                        workspace,
                        TransportOrigin::RetainedRelayDelta(author),
                        message.clone(),
                        None,
                        now,
                    )
                    .map_err(StoreLinkError::Replica)?;
                replayed = replayed.saturating_add(1);
            }
            let before = standing.clone();
            advance(&mut standing, &replay);
            if !replay.more || standing == before {
                return Ok(replayed);
            }
        }
    }

    /// Admit every frame the replica lacks, from the store, until it holds
    /// what the store has. Returns how many frames it admitted.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke, and
    /// [`StoreLinkError::Replica`] when the replica refuses a frame: the
    /// catch-up stops there, and the caller's cursors still stand where
    /// they did, so the next call asks for the refused frame again once
    /// what it needs has arrived.
    pub async fn catch_up(&mut self, sync: &DirectSync) -> Result<usize, StoreLinkError> {
        let mut standing = cursors(sync);
        let mut admitted = 0;
        loop {
            let replay = self.link.want(self.workspace, &standing).await?;
            if replay.frames.is_empty() {
                return Ok(admitted);
            }
            for message in &replay.frames {
                let author = author_of(message).ok_or(StoreLinkError::NotAFrame)?;
                sync.admit_retained(author, message.clone())
                    .map_err(StoreLinkError::Replica)?;
                admitted = admitted.saturating_add(1);
            }
            let before = standing.clone();
            advance(&mut standing, &replay);
            if !replay.more || standing == before {
                return Ok(admitted);
            }
        }
    }
}

/// The store's name for a workspace: what both sides name it by.
///
/// # Errors
///
/// Returns the reason when `id` is not a workspace id.
pub fn workspace_of(id: &str) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(id).map_err(|_| format!("{id} is not a workspace id"))
}

/// Where `sync`'s replica stands, one cursor per author line of its sealed
/// history: what it asks a store to continue from.
#[must_use]
pub fn cursors(sync: &DirectSync) -> Vec<Cursor> {
    sync.read(cursors_of)
}

/// Where `owner`'s copy of `workspace` stands, one cursor per author line of
/// its sealed history.
///
/// # Errors
///
/// The workspace is not held here or is fenced.
pub fn workspace_cursors(
    owner: &NativeWorkspaceOwner,
    workspace: WorkspaceId,
) -> Result<Vec<Cursor>, String> {
    owner.with_current(workspace, |view| Ok(cursors_of(view.peer())))
}

fn cursors_of(peer: &StatePeer) -> Vec<Cursor> {
    let workspace = peer.workspace_id().to_owned();
    peer.replay_floors()
        .into_iter()
        .map(|(app, record, author, seq)| Cursor {
            lineage: Lineage::of(&app, &workspace, &record, author),
            after_seq: seq,
        })
        .collect()
}

/// Whether `message` is a sealed document frame: the only history a store
/// keeps, since it never holds a workspace's keys.
#[must_use]
pub fn sealed(message: &NeighbourMsg) -> bool {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => envelope.encryption.is_some(),
        _ => false,
    }
}

/// The device that signed `message`, when it is a document frame.
#[must_use]
pub fn author_of(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(*envelope.author.as_bytes()),
        _ => None,
    }
}
