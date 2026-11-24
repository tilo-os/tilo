//! The resources of the workspaces a service holds, read for a device.
//!
//! The service's own copy of a workspace is the one its apps' Opens and the session's
//! own reads answer with: what its governor keeps of a document whole, sealed writes opened with
//! the content keys the device the service is on holds. Every read is decided at the resource's
//! node as a member's read is ([`tilo_peer_state::StatePeer::check_document_read`]), for the
//! device whose certificate asks.

#[cfg(feature = "apps")]
#[cfg(feature = "apps")]
use tilo_peer_state::PeerDocumentCapture;
#[cfg(feature = "apps")]
use tilo_peer_state::PeerDocumentReadError;
use tilo_peer_state::StatePeer;
#[cfg(feature = "apps")]
use tilo_user_identity::DeviceCert;

use super::{DeviceKeys, Workspaces};
use crate::documents::OpenedWith;
use tilo_lock::MutexRecover as _;
use tilo_peer_state::workspace_documents::DocumentRead;

impl Workspaces {
    /// `app`'s `resource` of `workspace` as this service's own device reads it: the document
    /// whole, or, for `since` heads this device holds, only what changed after them.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace, this device may not read the resource, it is sealed
    /// and this device holds no keys of it, its history does not open, or it exceeds `maximum`.
    pub(crate) async fn own_read(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        since: &[[u8; 32]],
        maximum: usize,
    ) -> Result<DocumentRead, String> {
        let moderation_pending = self.moderation_history_pending(workspace, app, resource);
        let (peer, device) = self
            .replica(workspace)
            .ok_or("this service does not hold the workspace")?;
        let reader = self.own_reader();
        if moderation_pending {
            // The read is decided as any other is, and waits for the history rather than
            // answering with a record the workspace has but this device has not read whole.
            peer.check_document_read(reader, app, resource, tilo_clock::wall())?;
            return Ok(DocumentRead::Pending);
        }
        let prepared =
            crate::documents::prepare(&peer, device.as_ref(), workspace, (app, resource), reader)
                .await?;
        // What this device opened of the resource before is read on, so a read opens only the
        // writes sealed since rather than the whole history again; a second read of it waits
        // for the first and reads on from what that one opened.
        let opening = self.inner.own_opened.of(workspace, app, resource);
        let mut held = opening.lock_recover();
        let mut kept = held.take();
        let read = crate::documents::finish(
            &peer,
            (app, resource),
            reader,
            since,
            maximum,
            prepared,
            &mut kept,
        );
        let size = kept
            .as_ref()
            .map_or(0, |(_, kept): &(OpenedWith, _)| kept.size());
        if kept.is_some() {
            *held = kept;
        }
        drop(held);
        self.inner.own_opened.kept((workspace, app, resource), size);
        read
    }

    /// Whether `app`'s `resource` of `workspace` reads for this service's own device now, decided
    /// as [`Self::own_read`] decides it before it opens anything: held here, its checkpoint and
    /// its keys arrived, and the sealed writes held cover its history. One this device may not
    /// read, or that does not open, waits for nothing: its read says why. Opening a sealed
    /// resource costs its whole history the first time after a start, seconds for a part of a
    /// large tracker, so nothing that only asks whether it could read one opens it.
    pub(crate) async fn own_readable(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
    ) -> bool {
        if self.moderation_history_pending(workspace, app, resource) {
            return false;
        }
        let Some((peer, device)) = self.replica(workspace) else {
            return false;
        };
        let reader = self.own_reader();
        crate::documents::readable(
            &peer,
            device.as_ref(),
            workspace,
            (app, resource),
            reader,
            tilo_clock::wall(),
        )
        .await
    }

    /// `workspace`'s policy as this device reads it, off the replica its own reads answer from:
    /// what a record opened for a person is read by ([`Self::own_read`]).
    pub fn policy_now(&self, workspace: &str) -> Option<tilo_action_policy::WorkspacePolicy> {
        let held = self.inner.readings.held(workspace)?;
        let reader = self.own_reader();
        let peer = held.replica.lock();
        peer.permissions(reader, tilo_clock::wall())
            .ok()
            .map(|permissions| (*permissions.policy).clone())
    }

    /// Every resource `workspace` publishes here, by its app and record: each document it holds,
    /// and the resources read from what it is rather than stored, who is in it
    /// ([`tilo_actions::remote::PRESENCE`]) and each key it admitted
    /// ([`tilo_actions::remote::KEY_GRANTS`]).
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    pub(crate) fn own_listing(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        use tilo_actions::remote::{KEY_GRANTS, PRESENCE};
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        Ok(held.replica.read(|peer| {
            let mut listed = crate::documents::listing(peer);
            listed.push((PRESENCE.to_owned(), String::new()));
            listed.extend(
                peer.governance()
                    .workspace_grants()
                    .into_iter()
                    .map(|grant| (KEY_GRANTS.to_owned(), grant.id)),
            );
            listed
        }))
    }

    /// Every document `workspace` holds here, by its app and route: what is stored of it, which
    /// a copy of the workspace takes.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    pub(crate) fn own_documents(&self, workspace: &str) -> Result<Vec<(String, String)>, String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        Ok(held.replica.read(crate::documents::listing))
    }

    /// The workspaces this service holds, by their ids.
    pub(crate) fn held_workspaces(&self) -> Vec<String> {
        self.inner.readings.workspaces()
    }

    /// The device this service reads its workspaces as: the one it is certified as, whose one
    /// key signs what it writes, serves its links and decides its reads.
    pub(super) fn own_reader(&self) -> [u8; 32] {
        self.device_cert().device_public_key
    }

    /// Whether `workspace` seals every write of its resources: its device takes part in the
    /// workspace's key group and holds its keys.
    #[cfg(feature = "apps")]
    pub(crate) fn seals_resources(&self, workspace: &str) -> bool {
        self.inner
            .readings
            .held(workspace)
            .is_some_and(|held| held.device.is_some())
    }

    /// Whether this service holds anything of `app`'s `resource` in `workspace`, committed or
    /// sealed alike.
    pub(crate) fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.inner.readings.held(workspace).is_some_and(|held| {
            held.moderation_pending(app, resource)
                || held
                    .replica
                    .read(|peer| crate::documents::holds(peer, app, resource))
        })
    }

    /// The revision `app`'s `resource` of `workspace` stands at for the device `reader`: decided
    /// as a read is, handing nothing out. `None` while a checkpoint is arriving, which only a
    /// capture answers.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, or it is sealed and this device
    /// holds no keys of it.
    #[cfg(feature = "apps")]
    pub(crate) async fn resource_revision(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
    ) -> Result<Option<[u8; 32]>, String> {
        if self.moderation_history_pending(workspace, app, resource) {
            return Ok(None);
        }
        let (peer, device) = self
            .replica(workspace)
            .ok_or("this service does not hold the workspace")?;
        crate::documents::revision(
            &peer,
            device.as_ref(),
            workspace,
            (app, resource),
            reader,
            tilo_clock::wall(),
        )
        .await
    }

    /// Whether `app`'s `resource` of `workspace` is still arriving: its governed checkpoint is
    /// cut in flight, so it reads once that body is admitted.
    #[cfg(feature = "apps")]
    pub(crate) fn resource_arriving(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.inner.readings.held(workspace).is_some_and(|held| {
            held.moderation_pending(app, resource)
                || held
                    .replica
                    .read(|peer| peer.checkpoint_pending(app, resource))
        })
    }

    /// Whether the workspace's legacy Chat reports and decisions have not been read into its
    /// moderation record yet: a read of that record waits for them rather than answering with
    /// what this device has not imported.
    pub(super) fn moderation_history_pending(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        if (app, resource) != (tilo_app_id::apps::CHAT, moderation_model::DOCUMENT) {
            return false;
        }
        self.inner
            .readings
            .held(workspace)
            .is_some_and(|held| held.moderation_pending(app, resource))
    }

    /// Whether `certificate`'s device may read `app`'s `resource` of `workspace` now: a device the
    /// workspace certifies, a current member, and the resource's Read permission as its governance
    /// grants it.
    #[cfg(feature = "apps")]
    pub(crate) fn authorizes_resource_read(
        &self,
        workspace: &str,
        certificate: &DeviceCert,
        app: &str,
        resource: &str,
    ) -> Result<(), String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        let peer = held.replica.lock();
        let certified = peer.certificates().iter().any(|known| {
            known.device_public_key == certificate.device_public_key
                && known.user_public_key == certificate.user_public_key
        });
        if !certified {
            return Err("the reader is no device the workspace certifies".to_owned());
        }
        let checked = peer.check_document_read(
            certificate.device_public_key,
            app,
            resource,
            tilo_clock::wall(),
        );
        drop(peer);
        checked
    }

    /// `app`'s `resource` of `workspace` as the device `reader` reads it now: at most `maximum`
    /// bytes of it, its sealed writes opened with the service's content keys. `None` while it is
    /// on its way: sealed under keys this device is due and has not taken yet, or missing a write
    /// this device knows of. It reads once those arrive.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, it is sealed for a group this device
    /// takes no part in, its history does not open, or it exceeds `maximum`.
    #[cfg(feature = "apps")]
    pub(crate) async fn capture_resource(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
        maximum: usize,
    ) -> Result<Option<PeerDocumentCapture>, PeerDocumentReadError> {
        let (peer, device) = self
            .replica(workspace)
            .ok_or(PeerDocumentReadError::Missing)?;
        crate::documents::capture(
            &peer,
            device.as_ref(),
            workspace,
            (app, resource),
            reader,
            tilo_clock::wall(),
            maximum,
        )
        .await
    }

    /// Whether a part of the chain `app`'s `resource` names is held here: the answer an Open of a
    /// chain's head is decided by.
    #[cfg(feature = "apps")]
    pub(crate) fn chain_head_has_part(&self, workspace: &str, app: &str, resource: &str) -> bool {
        let Ok(routes) = self.own_listing(workspace) else {
            return false;
        };
        routes.into_iter().any(|(other_app, other)| {
            other_app == app
                && matches!(
                    neighbour_wire::document_sync(&other),
                    neighbour_wire::DocumentSync::Part { base, .. }
                        if part_of(resource) == Some(base)
                )
        })
    }

    /// The replica of `workspace` and the content keys its device holds, taken in a moment: the
    /// governor stays free while a read opens a sealed history.
    fn replica(&self, workspace: &str) -> Option<(StatePeer, Option<DeviceKeys>)> {
        let held = self.inner.readings.held(workspace)?;
        Some((held.replica.read(StatePeer::clone), held.device))
    }
}

/// The chain `resource`, a chain's head, belongs to; `None` for any other resource.
#[cfg(feature = "apps")]
fn part_of(resource: &str) -> Option<&str> {
    match neighbour_wire::document_sync(resource) {
        neighbour_wire::DocumentSync::ChainHead { base } => Some(base),
        _ => None,
    }
}

/// Sealed resources as this service's own device last opened them ([`crate::documents::Opened`]).
pub(super) use crate::documents::Opened as OwnOpened;
