//! A room's durable records, for a service the host process embeds.
//!
//! A page asks the host to run an app action on a record nobody has open.
//! The service answering it (the app-serving plane) must read the record as
//! the host retains it and commit the action's change the way a device
//! commits one: ordered, retained, fanned out to every subscriber and queued
//! for offline members. [`RoomRecords`] is that seam, without a network hop:
//!
//! - [`RoomRecords::retained`] is what a first-time subscriber is replayed —
//!   every retained checkpoint and delta of the workspace, as the signed
//!   document bodies they carry with their authenticated authors;
//! - [`RoomRecords::commit`] admits one signed document body authored by this
//!   host's own device, exactly as a device's `Commit` is admitted.
//!
//! The host signs as itself; a page opening the frame checks the signature
//! against the author the host stamped, as for any device.

use tilo_clock::unix_now_ns;

use crate::coedit::CoeditPayload;
use crate::peer::WorkspaceRegistry;
use crate::transport::Envelope;

/// One retained document body of a workspace, as its author signed it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetainedBody {
    /// The device the host authenticated when it admitted the body.
    pub author: [u8; 32],
    /// The encoded signed document message.
    pub body: Vec<u8>,
}

/// One signed document body to admit as this host's own change.
#[derive(Clone, Debug)]
pub struct HostCommit {
    /// The encoded signed document message.
    pub body: Vec<u8>,
    /// Its retention channel.
    pub channel: [u8; 32],
    /// Whether it replaces the channel's checkpoint.
    pub checkpoint: bool,
}

/// In-process access to the workspaces this relay hosts.
#[derive(Clone)]
pub struct RoomRecords {
    workspaces: WorkspaceRegistry,
    device: [u8; 32],
    user: [u8; 32],
    signing_key: ed25519_dalek::SigningKey,
    metrics: std::sync::Arc<crate::metrics::Metrics>,
}

impl RoomRecords {
    pub(crate) const fn new(
        workspaces: WorkspaceRegistry,
        device: [u8; 32],
        user: [u8; 32],
        signing_key: ed25519_dalek::SigningKey,
        metrics: std::sync::Arc<crate::metrics::Metrics>,
    ) -> Self {
        Self {
            workspaces,
            device,
            user,
            signing_key,
            metrics,
        }
    }

    /// This host's device key: the author of what [`Self::commit`] admits.
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

    /// Every retained document body of `workspace_id`, checkpoints before
    /// the deltas retained after them.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or could not be read.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard only lives for the lookup; the value kept is a clone"
    )]
    pub async fn retained(&self, workspace_id: u64) -> Result<Vec<RetainedBody>, String> {
        let ws = self
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or("the workspace is not hosted here")?;
        let encoded = {
            let guard = ws.lock().await;
            guard
                .store
                .load_retained()
                .map_err(|error| error.to_string())?
        };
        Ok(encoded
            .iter()
            // Retained entries are whole frames: a length prefix, then the
            // envelope.
            .filter_map(|raw| match crate::frame::decode(raw.get(4..)?).ok()? {
                Envelope::Delta {
                    author_device_public_key,
                    delta,
                    ..
                } => match CoeditPayload::decode(&delta)? {
                    CoeditPayload::Checkpoint { body, .. }
                    | CoeditPayload::RetainedDelta { body, .. } => Some(RetainedBody {
                        author: author_device_public_key,
                        body,
                    }),
                },
                _ => None,
            })
            .collect())
    }

    /// Admit `commit` to `workspace_id` as this host's own change: ordered,
    /// retained, fanned out to every subscriber and queued for offline
    /// members. Returns its host order.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or the change could not
    /// be persisted.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard only lives for the lookup; the value kept is a clone"
    )]
    pub async fn commit(&self, workspace_id: u64, commit: HostCommit) -> Result<u64, String> {
        let payload = if commit.checkpoint {
            CoeditPayload::Checkpoint {
                channel: commit.channel,
                body: commit.body,
            }
        } else {
            CoeditPayload::RetainedDelta {
                channel: commit.channel,
                body: commit.body,
            }
        };
        let delta = payload.encode().map_err(|error| error.to_string())?;
        let ws = self
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or("the workspace is not hosted here")?;
        let envelope = Envelope::Delta {
            workspace_id,
            author_device_public_key: self.device,
            authored_at_ns: unix_now_ns(),
            delta,
            signature: [0; 64],
        };
        crate::peer::admit_host_state(&ws, envelope, std::sync::Arc::clone(&self.metrics)).await
    }
}
