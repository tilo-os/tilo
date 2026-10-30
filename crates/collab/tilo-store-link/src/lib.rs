//! A member device's side of a workspace store.
//!
//! The device's replica ([`DirectSync`]) is the member's own history; a
//! store is the copy another device of the workspace keeps for it while it
//! is away. This crate joins the two:
//!
//! * it keeps what the device publishes at the store ([`StoreSession::keep`]),
//!   which the store retains sealed and can never read;
//! * it catches the replica up from the store on start
//!   ([`StoreSession::catch_up`]): it asks from the replica's own cursors
//!   ([`cursors`]), and admits every replayed frame as its author's retained
//!   history, the same admission a live frame from that device takes.
//!
//! Membership is not decided here: the store admits the connecting device
//! by the [`Access`](tilo_store_node::Access) its embedder gave it, and the
//! replica refuses a frame the workspace's roster does not back.

#![deny(rust_2018_idioms)]

use iroh::EndpointAddr;
use neighbour_wire::NeighbourMsg;
use tilo_peer_state::direct::DirectSync;
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

    /// Keep `message` at the store. Returns whether it was a document frame
    /// worth keeping; presence, surfaces and other live traffic is not
    /// history and is not sent.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke or the store
    /// refused.
    pub async fn keep(&mut self, message: &NeighbourMsg) -> Result<bool, StoreLinkError> {
        if author_of(message).is_none() {
            return Ok(false);
        }
        self.link
            .push(self.workspace, std::slice::from_ref(message))
            .await?;
        Ok(true)
    }

    /// Admit every frame the replica lacks, from the store, until it holds
    /// what the store has. Returns how many frames it admitted.
    ///
    /// # Errors
    ///
    /// [`StoreLinkError::Store`] when the session broke, and
    /// [`StoreLinkError::Replica`] when the replica refuses a frame: the
    /// caller keeps the session and carries on, since the frames after a
    /// refused one may still land.
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

/// The workspace a room's store holds: what both sides name a workspace by.
///
/// # Errors
///
/// Returns the reason when `room` is not a room id.
pub fn workspace_of(room: &str) -> Result<u64, String> {
    tilo_relay_protocol::room_workspace_id(room).map_err(|_| format!("{room} is not a room id"))
}

/// Where `sync`'s replica stands, one cursor per author line of its sealed
/// history: what it asks a store to continue from.
#[must_use]
pub fn cursors(sync: &DirectSync) -> Vec<Cursor> {
    sync.read(|peer| {
        let room = peer.room().to_owned();
        peer.replay_floors()
            .into_iter()
            .map(|(app, record, author, seq)| Cursor {
                lineage: Lineage::of(&app, &room, &record, author),
                after_seq: seq,
            })
            .collect()
    })
}

/// The device that signed `message`, when it is a document frame.
#[must_use]
pub fn author_of(message: &NeighbourMsg) -> Option<[u8; 32]> {
    match message {
        NeighbourMsg::AppDocument { envelope, .. } => Some(*envelope.author.as_bytes()),
        _ => None,
    }
}
