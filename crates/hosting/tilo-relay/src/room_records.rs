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
    inflight: std::sync::Arc<Inflight>,
}

/// The host commits each workspace has queued that are not durable yet.
/// A caller may queue a commit and wait for it later, off its own lock;
/// whoever reads the workspace's retention meanwhile must still see it.
#[derive(Default)]
struct Inflight {
    open: std::sync::Mutex<std::collections::HashMap<u64, Open>>,
    settled: tokio::sync::Notify,
}

#[derive(Default)]
struct Open {
    next: u64,
    queued: std::collections::BTreeSet<u64>,
}

impl Inflight {
    fn lock(&self) -> std::sync::MutexGuard<'_, std::collections::HashMap<u64, Open>> {
        self.open
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn queue(self: &std::sync::Arc<Self>, workspace_id: u64) -> Ticket {
        let mut open = self.lock();
        let entry = open.entry(workspace_id).or_default();
        let number = entry.next;
        entry.next = entry.next.wrapping_add(1);
        entry.queued.insert(number);
        drop(open);
        Ticket {
            inflight: std::sync::Arc::clone(self),
            workspace_id,
            number,
        }
    }

    /// Whether every commit queued for `workspace_id` before `next` is
    /// durable (or abandoned).
    fn settled_before(&self, workspace_id: u64, next: u64) -> bool {
        self.lock()
            .get(&workspace_id)
            .is_none_or(|open| open.queued.first().is_none_or(|first| *first >= next))
    }

    /// Wait until every commit queued for `workspace_id` so far is durable.
    async fn settle(&self, workspace_id: u64) {
        let Some(next) = self.lock().get(&workspace_id).map(|open| open.next) else {
            return;
        };
        loop {
            let notified = self.settled.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.settled_before(workspace_id, next) {
                return;
            }
            notified.await;
        }
    }
}

/// One queued host commit, until it is durable or given up.
struct Ticket {
    inflight: std::sync::Arc<Inflight>,
    workspace_id: u64,
    number: u64,
}

impl Drop for Ticket {
    fn drop(&mut self) {
        let mut open = self.inflight.lock();
        if let Some(entry) = open.get_mut(&self.workspace_id) {
            entry.queued.remove(&self.number);
            if entry.queued.is_empty() {
                open.remove(&self.workspace_id);
            }
        }
        drop(open);
        self.inflight.settled.notify_waiters();
    }
}

/// A host commit holding its place in the workspace's order, not yet
/// waited on: [`RoomRecords::retained`] waits for it until it is durable.
pub(crate) struct RoomAdmission {
    admission: crate::peer::HostAdmission,
    _ticket: Ticket,
}

impl RoomAdmission {
    /// Wait until the commit is durable and published; returns its host
    /// order.
    pub(crate) async fn durable(self) -> Result<u64, String> {
        self.admission.durable().await
    }
}

impl RoomRecords {
    pub(crate) fn new(
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
            inflight: std::sync::Arc::default(),
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
    /// the deltas retained after them, including every host commit queued
    /// before the call.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or could not be read.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard only lives for the lookup; the value kept is a clone"
    )]
    pub async fn retained(&self, workspace_id: u64) -> Result<Vec<RetainedBody>, String> {
        self.inflight.settle(workspace_id).await;
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

    /// Bounded existing ciphertext for one channel, without changing retention.
    #[cfg(feature = "agents")]
    pub(crate) async fn view_channel(
        &self,
        workspace: u64,
        channel: [u8; 32],
        max_bytes: usize,
    ) -> Result<Vec<RetainedBody>, String> {
        self.inflight.settle(workspace).await;
        let ws = self
            .workspaces
            .lock()
            .await
            .get(&workspace)
            .cloned()
            .ok_or("the workspace is not hosted here")?;
        let guard = ws.lock().await;
        let encoded = guard
            .store
            .load_view_channel(&channel, max_bytes)
            .map_err(|error| error.to_string())?;
        drop(guard);
        drop(ws);
        encoded
            .into_iter()
            .map(|raw| {
                let (author, payload) = crate::store::retained::payload(&raw, workspace)
                    .ok_or("invalid retained source frame")?;
                let body = match payload {
                    CoeditPayload::Checkpoint { body, .. }
                    | CoeditPayload::RetainedDelta { body, .. } => body,
                };
                Ok(RetainedBody { author, body })
            })
            .collect()
    }

    /// Admit `commit` to `workspace_id` as this host's own change: ordered,
    /// retained, fanned out to every subscriber and queued for offline
    /// members. Returns its host order.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or the change could not
    /// be persisted.
    pub async fn commit(&self, workspace_id: u64, commit: HostCommit) -> Result<u64, String> {
        self.enqueue(workspace_id, commit).await?.durable().await
    }

    /// [`Self::commit`] without waiting for durability: the commit holds its
    /// place in the workspace's order once this returns.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or the change could not
    /// be queued.
    pub(crate) async fn enqueue(
        &self,
        workspace_id: u64,
        commit: HostCommit,
    ) -> Result<RoomAdmission, String> {
        let reservation = self.reserve(workspace_id);
        self.enqueue_reserved(workspace_id, commit, reservation)
            .await
    }

    /// Hold a place for a host commit of `workspace_id` that is admitted
    /// later, with [`Self::enqueue_reserved`]: from now on
    /// [`Self::retained`] waits for it as for a commit already queued,
    /// until it is durable or the reservation is dropped.
    pub(crate) fn reserve(&self, workspace_id: u64) -> Reservation {
        Reservation(self.inflight.queue(workspace_id))
    }

    /// [`Self::enqueue`] for a commit whose place `reservation` holds.
    ///
    /// # Errors
    ///
    /// Returns why the workspace is not hosted here or the change could not
    /// be queued.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard only lives for the lookup; the value kept is a clone"
    )]
    pub(crate) async fn enqueue_reserved(
        &self,
        workspace_id: u64,
        commit: HostCommit,
        reservation: Reservation,
    ) -> Result<RoomAdmission, String> {
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
        let admission =
            crate::peer::enqueue_host_state(&ws, envelope, std::sync::Arc::clone(&self.metrics))
                .await?;
        Ok(RoomAdmission {
            admission,
            _ticket: reservation.0,
        })
    }
}

/// A host commit [`RoomRecords::retained`] already waits for, not yet in
/// its workspace's order.
pub(crate) struct Reservation(Ticket);
