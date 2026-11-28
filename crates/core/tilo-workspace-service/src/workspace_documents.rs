//! A workspace's kept records, for a service the host process embeds.
//!
//! A page asks the host to run an app action on a record nobody has open.
//! The service answering it (the app action server) must read the record as
//! the host keeps it and write the action's change the way a device writes
//! one: kept at the device's store and carried to every device on the
//! workspace's link. [`WorkspaceDocuments`] is that boundary, without a network hop:
//!
//! - [`WorkspaceDocuments::retained`] is what a device that links is replayed:
//!   each channel's governance, then every kept write of the workspace, as the
//!   signed bodies they carry with their authors;
//! - [`WorkspaceDocuments::commit`] keeps and carries one signed body this
//!   host's own device wrote.
//!
//! The host signs as itself; a device opening the frame checks the signature
//! against its author, as for any device.

#[cfg(not(target_arch = "wasm32"))]
use crate::registry::WorkspaceRegistry;
#[cfg(not(target_arch = "wasm32"))]
use neighbour_wire::NeighbourMsg;
#[cfg(not(target_arch = "wasm32"))]
use tilo_server_protocol::WorkspaceId;

#[cfg(not(target_arch = "wasm32"))]
/// One kept body of a workspace, as its author signed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedBody {
    /// The device that signed it.
    pub author: [u8; 32],
    /// The encoded signed message.
    pub body: Vec<u8>,
}

#[cfg(not(target_arch = "wasm32"))]
/// One signed body this host's own device wrote, to keep and carry.
#[derive(Clone, Debug)]
pub struct HostCommit {
    /// The encoded signed message.
    pub body: Vec<u8>,
}

/// What a device's own roles read and write its workspaces' documents through, shared with
/// every process that holds documents for them: the service implements it here, and a page
/// implements it over its replicated copy.
pub use tilo_peer_state::workspace_documents::{
    DeviceDocuments, DeviceWrites, DocumentRead, DocumentServed, DocumentServing, DocumentWrite,
};

#[cfg(not(target_arch = "wasm32"))]
/// In-process access to the workspaces this server keeps.
#[derive(Clone)]
pub struct WorkspaceDocuments {
    workspaces: WorkspaceRegistry,
    store: std::sync::Arc<tilo_peer_state::store::Store>,
    direct: tilo_peer_state::direct::Serving,
    device: [u8; 32],
    user: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
}

#[cfg(not(target_arch = "wasm32"))]
impl WorkspaceDocuments {
    pub(crate) const fn new(
        workspaces: WorkspaceRegistry,
        store: std::sync::Arc<tilo_peer_state::store::Store>,
        direct: tilo_peer_state::direct::Serving,
        device: [u8; 32],
        user: [u8; 32],
        signing_key: ed25519_dalek::SigningKey,
    ) -> Self {
        Self {
            workspaces,
            store,
            direct,
            device,
            user,
            signing_key,
        }
    }

    /// The next `count` sequence numbers of this device's own writes of `record` of `app` in
    /// `workspace`, the first of them: none at or below `floor`, the last the record gave the
    /// device's line. The block they come from is durable before this answers
    /// ([`tilo_peer_state::store::Store::reserve_sequence`]).
    ///
    /// # Errors
    ///
    /// The block could not be written.
    pub(crate) fn reserve_sequence(
        &self,
        workspace: WorkspaceId,
        app: &str,
        record: &str,
        author: [u8; 32],
        floor: u64,
        count: u64,
    ) -> Result<u64, String> {
        let lineage = tilo_peer_state::store::Lineage::of(app, record, author);
        self.store
            .reserve_sequence(workspace, lineage, floor, count)
            .map_err(|error| error.to_string())
    }

    /// This host's device key: the author of what [`Self::commit`] keeps.
    #[must_use]
    pub const fn device(&self) -> [u8; 32] {
        self.device
    }

    /// The user root this host chains to.
    #[must_use]
    pub const fn user_public_key(&self) -> [u8; 32] {
        self.user
    }

    /// The key [`HostCommit`] bodies must be signed with.
    #[must_use]
    pub const fn signing_key(&self) -> &ed25519_dalek::SigningKey {
        &self.signing_key
    }

    /// Every kept body of `workspace_id`: each channel's governance and each
    /// change to the workspace's own state, in the order they were kept, then
    /// every kept write, in the store's order.
    ///
    /// # Errors
    ///
    /// `workspace_id` is not a workspace this host keeps, or a kept frame
    /// could not be read back.
    pub async fn retained(&self, workspace_id: WorkspaceId) -> Result<Vec<RetainedBody>, String> {
        // A workspace this host does not keep has nothing to replay, and a
        // caller must be able to tell that from a workspace that is here and
        // holds nothing yet. The registry is what keeps: a member copy this
        // device holds syncs with the device serving it, and is no less kept
        // for having no link of its own. The registry is let go before the
        // store is read.
        let kept = self.workspaces.lock().await.contains_key(&workspace_id);
        if !kept {
            return Err("the workspace is not hosted here".into());
        }
        let mut bodies = Vec::new();
        for (author, message) in self.store.channels(workspace_id) {
            bodies.push(body_of(author, &message)?);
        }
        let mut cursors = Vec::new();
        loop {
            let replay = self
                .store
                .replay(
                    workspace_id,
                    &cursors,
                    tilo_peer_state::store::Budget::default(),
                )
                .map_err(|error| error.to_string())?;
            tilo_peer_state::store::advance(&mut cursors, &replay.frames);
            for message in &replay.frames {
                let author = tilo_peer_state::store::author_of(message).unwrap_or_default();
                bodies.push(body_of(author, message)?);
            }
            if !replay.more {
                return Ok(bodies);
            }
        }
    }

    /// Every kept body of `workspace_id`, as [`Self::retained`] once the
    /// writes this host made are durable: the store is durable when it
    /// answers, so this is the same read.
    ///
    /// # Errors
    ///
    /// As [`Self::retained`].
    pub(crate) async fn retained_settled(
        &self,
        workspace_id: WorkspaceId,
    ) -> Result<Vec<RetainedBody>, String> {
        self.retained(workspace_id).await
    }

    /// The kept bodies of `channel`, one record's route, up to `max_bytes` of them.
    ///
    /// # Errors
    ///
    /// As [`Self::retained`].
    #[cfg(feature = "agents")]
    pub async fn view_channel(
        &self,
        workspace: WorkspaceId,
        channel: [u8; 32],
        max_bytes: usize,
    ) -> Result<Vec<RetainedBody>, String> {
        let mut bytes = 0_usize;
        let mut kept = Vec::new();
        for retained in self.retained(workspace).await? {
            let Ok(message) = NeighbourMsg::decode(&retained.body) else {
                continue;
            };
            if neighbour_wire::retention(&message, false).map(|(route, _)| route) != Some(channel) {
                continue;
            }
            bytes = bytes.saturating_add(retained.body.len());
            if bytes > max_bytes {
                break;
            }
            kept.push(retained);
        }
        Ok(kept)
    }

    /// Keep `commit`, a body this host's own device signed, and carry it to
    /// every device on `workspace_id`'s link.
    ///
    /// Answers the host order it was kept at.
    ///
    /// # Errors
    ///
    /// The body does not decode, or the store could not keep it.
    pub fn commit(&self, workspace_id: WorkspaceId, commit: &HostCommit) -> Result<u64, String> {
        self.carry(workspace_id, self.device, commit)
    }

    /// Keep `commit`, `author`'s signed body, and carry it to every device on
    /// `workspace_id`'s link: live to those linked now, and kept for those that
    /// link later.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`].
    pub(crate) fn carry(
        &self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        commit: &HostCommit,
    ) -> Result<u64, String> {
        let message = NeighbourMsg::decode(&commit.body).map_err(|error| error.to_string())?;
        if let Some(link) = self.direct.link(workspace_id) {
            link.carry(author, &message)?;
            return Ok(host_order());
        }
        if let Ok(frame) = tilo_peer_state::store::Frame::of(&message) {
            self.store
                .retain(workspace_id, &frame)
                .map_err(|error| error.to_string())?;
        } else if message.governance_workspace().is_some() {
            self.store
                .keep_channel(workspace_id, author, &message)
                .map_err(|error| error.to_string())?;
        } else {
            self.store
                .keep_workspace_change(workspace_id, author, &message)
                .map_err(|error| error.to_string())?;
        }
        Ok(host_order())
    }

    /// [`Self::carry`] each of `messages`, `author`'s, in order: the frames among them are
    /// kept with one sync between them, so a batch of writes waits for the disk once.
    ///
    /// # Errors
    ///
    /// As [`Self::commit`]; the messages before the one that failed are kept.
    pub(crate) fn carry_all(
        &self,
        workspace_id: WorkspaceId,
        author: [u8; 32],
        messages: &[NeighbourMsg],
    ) -> Result<(), String> {
        if let Some(link) = self.direct.link(workspace_id) {
            return link.carry_all(author, messages).map(drop);
        }
        let mut frames = Vec::new();
        for message in messages {
            if let Ok(frame) = tilo_peer_state::store::Frame::of(message) {
                frames.push(frame);
                continue;
            }
            // A message kept elsewhere is kept after the frames before it.
            self.store
                .retain_all(workspace_id, &std::mem::take(&mut frames))
                .map_err(|error| error.to_string())?;
            let body = postcard::to_stdvec(message).map_err(|error| error.to_string())?;
            self.carry(workspace_id, author, &HostCommit { body })?;
        }
        self.store
            .retain_all(workspace_id, &frames)
            .map_err(|error| error.to_string())?;
        Ok(())
    }
}

/// The host order of a write kept now: wall-clock nanoseconds, forced strictly
/// past the previous one so a clock step never reorders.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn host_order() -> u64 {
    static LAST: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let now = tilo_clock::wall()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            u64::try_from(since.as_nanos()).unwrap_or(u64::MAX)
        });
    let previous = LAST
        .fetch_update(
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
            |last| Some(now.max(last.saturating_add(1))),
        )
        .unwrap_or_default();
    now.max(previous.saturating_add(1))
}

/// `message`, `author`'s, as a kept body.
#[cfg(not(target_arch = "wasm32"))]
fn body_of(author: [u8; 32], message: &NeighbourMsg) -> Result<RetainedBody, String> {
    Ok(RetainedBody {
        author,
        body: postcard::to_allocvec(message).map_err(|error| error.to_string())?,
    })
}
