//! The resources of the workspaces a service holds, read for a device.
//!
//! The service's own copy of a workspace is the one its apps' Opens and the session's
//! own reads answer with: what its governor keeps of a document whole, sealed writes opened with
//! the content keys the device the service is on holds. Every read is decided at the resource's
//! node as a member's read is ([`tilo_peer_state::StatePeer::check_document_read`]), for the
//! device whose certificate asks.

use automerge::ReadDoc as _;
use collab_types::OwnResource;
use tilo_peer_state::{
    KeptDocument, PeerDocumentCapture, PeerDocumentReadError, StatePeer, changes_since,
    committed_document_revision, saved_whole,
};
use tilo_user_identity::DeviceCert;

use super::{DeviceKeys, GovernedWorkspace, SystemTime, Workspaces};
use crate::workspace_documents::DocumentRead;

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
        let (peer, device) = self
            .replica(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        if !peer.holds_document(app, resource) {
            return Ok(DocumentRead::Absent);
        }
        if peer.checkpoint_pending(app, resource) {
            return Ok(DocumentRead::Pending);
        }
        let reader = self
            .own_reader(workspace)
            .await
            .ok_or("this service acts as no device here")?;
        let keys = content_keys(&peer, device.as_ref(), workspace, (app, resource))
            .await
            .map_err(|error| error.to_string())?;
        let mut opened = keys
            .as_ref()
            .map(super::sealed_documents::DocumentKeys::open);
        let mut kept = KeptDocument::default();
        let document = peer
            .open_document(
                reader,
                (app, resource),
                tilo_clock::wall(),
                maximum,
                opened.as_mut().map(|open| {
                    open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>
                }),
                &mut kept,
            )
            .map_err(|error| error.to_string())?;
        let heads: Vec<[u8; 32]> = document.get_heads().iter().map(|head| head.0).collect();
        let revision = committed_document_revision(&document.get_heads());
        // A reader's heads this device does not hold are answered with the whole document: the
        // changes it would send are not the complete ones after them.
        let holds_since = since.iter().all(|head| {
            document
                .get_change_by_hash(&automerge::ChangeHash(*head))
                .is_some()
        });
        if since.is_empty() || !holds_since {
            let bytes = saved_whole(document, maximum).map_err(|error| error.to_string())?;
            return Ok(DocumentRead::Whole {
                revision,
                heads,
                bytes,
            });
        }
        let since: Vec<automerge::ChangeHash> = since
            .iter()
            .map(|head| automerge::ChangeHash(*head))
            .collect();
        let bytes = changes_since(document, &since, maximum).map_err(|error| error.to_string())?;
        if bytes.is_empty() && heads == since.iter().map(|head| head.0).collect::<Vec<_>>() {
            return Ok(DocumentRead::Unchanged(revision));
        }
        Ok(DocumentRead::Changes {
            revision,
            heads,
            bytes,
        })
    }

    /// Every document `workspace` holds here, by its app and route.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the governor's guard lives only while its routes are copied out"
    )]
    pub(crate) async fn own_listing(
        &self,
        workspace: &str,
    ) -> Result<Vec<(String, String)>, String> {
        let held = self
            .held_workspace(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        let listing = {
            let held = held.lock().await;
            let peer = held.peer();
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
        };
        Ok(listing)
    }

    /// The workspaces this service holds, by their ids.
    pub(crate) async fn held_workspaces(&self) -> Vec<String> {
        self.inner.workspaces.lock().await.keys().cloned().collect()
    }

    /// The device this service reads `workspace` as: its own where it governs the workspace, the
    /// device its member copy acts as where it joins one.
    async fn own_reader(&self, workspace: &str) -> Option<[u8; 32]> {
        let held = self.held_workspace(workspace).await?;
        let member = held.lock().await.member.is_some();
        if member {
            return self.member_identity().ok().map(|device| device.device());
        }
        Some(self.governor().device_public_key)
    }

    /// Whether `workspace` seals every write of its resources: its device takes part in the
    /// workspace's key group and holds its keys.
    pub(crate) async fn seals_resources(&self, workspace: &str) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.seals(),
            None => false,
        }
    }

    /// The member's own resource `app`'s `resource` in `workspace` is, when it is one.
    pub(crate) async fn own_resource(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> Option<OwnResource> {
        let held = self.held_workspace(workspace).await?;
        let held = held.lock().await;
        held.peer().own_resource(app, resource)
    }

    /// Whether this service holds anything of `app`'s `resource` in `workspace`, committed or
    /// sealed alike.
    pub(crate) async fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.peer().holds_document(app, resource),
            None => false,
        }
    }

    /// Whether this service holds `workspace` itself, governed or as a member copy.
    pub(crate) async fn holds_workspace(&self, workspace: &str) -> bool {
        self.inner.workspaces.lock().await.contains_key(workspace)
    }

    /// The revision `app`'s `resource` of `workspace` stands at for the device `reader`: decided
    /// as a read is, handing nothing out. `None` while a checkpoint is arriving, which only a
    /// capture answers.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, or it is sealed and this device
    /// holds no keys of it.
    pub(crate) async fn resource_revision(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
    ) -> Result<Option<[u8; 32]>, String> {
        let (peer, device) = self
            .replica(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        resource_revision(
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
    pub(crate) async fn resource_arriving(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        match self.held_workspace(workspace).await {
            Some(held) => held.lock().await.peer().checkpoint_pending(app, resource),
            None => false,
        }
    }

    /// Whether `certificate`'s device may read `app`'s `resource` of `workspace` now: a device the
    /// workspace certifies, a current member, and the resource's Read permission as its governance
    /// grants it.
    pub(crate) async fn authorizes_resource_read(
        &self,
        workspace: &str,
        certificate: &DeviceCert,
        app: &str,
        resource: &str,
    ) -> Result<(), String> {
        let held = self
            .held_workspace(workspace)
            .await
            .ok_or("this service does not hold the workspace")?;
        let guard = held.lock().await;
        let peer = guard.peer();
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
        drop(guard);
        checked
    }

    /// `app`'s `resource` of `workspace` as the device `reader` reads it now: at most `maximum`
    /// bytes of it, its sealed writes opened with the service's content keys.
    ///
    /// # Errors
    ///
    /// The reader may not read it, the service holds no copy, it is sealed and this device holds
    /// no keys of it, its history does not open, or it exceeds `maximum`.
    pub(crate) async fn capture_resource(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: [u8; 32],
        maximum: usize,
    ) -> Result<PeerDocumentCapture, PeerDocumentReadError> {
        let (peer, device) = self
            .replica(workspace)
            .await
            .ok_or(PeerDocumentReadError::Missing)?;
        capture_resource(
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
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the replica's guard is held only while its routes are copied out"
    )]
    pub(crate) async fn chain_head_has_part(
        &self,
        workspace: &str,
        app: &str,
        resource: &str,
    ) -> bool {
        let Some(held) = self.held_workspace(workspace).await else {
            return false;
        };
        let routes: Vec<_> = {
            let held = held.lock().await;
            let peer = held.peer();
            peer.plaintext_routes()
                .into_iter()
                .chain(peer.sealed_routes())
                .collect()
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
    async fn replica(&self, workspace: &str) -> Option<(StatePeer, Option<DeviceKeys>)> {
        let held = self.held_workspace(workspace).await?;
        let held = held.lock().await;
        Some((held.peer().clone(), held.device.clone()))
    }

    /// The bit of `workspace`'s governor the reads above ask: its replica, or `None` for a
    /// workspace not governed here.
    async fn held_workspace(
        &self,
        workspace: &str,
    ) -> Option<std::sync::Arc<tokio::sync::Mutex<GovernedWorkspace>>> {
        self.workspace(workspace).await.ok()
    }
}

/// The chain `resource`, a chain's head, belongs to; `None` for any other resource.
fn part_of(resource: &str) -> Option<&str> {
    match neighbour_wire::document_sync(resource) {
        neighbour_wire::DocumentSync::ChainHead { base } => Some(base),
        _ => None,
    }
}

/// The content keys `app`'s `resource` of `workspace` is sealed with, when it is sealed and this
/// device holds them.
async fn content_keys(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
) -> Result<Option<super::sealed_documents::DocumentKeys>, PeerDocumentReadError> {
    if !peer.publication().is_sealed_document(app, resource) {
        return Ok(None);
    }
    let Some(device) = device else {
        return Err(PeerDocumentReadError::Encrypted);
    };
    match device
        .group_keys(workspace, collab_types::EVERY_MEMBER, false)
        .await
    {
        None => Err(PeerDocumentReadError::Encrypted),
        Some(Err(error)) => Err(PeerDocumentReadError::Opening(error)),
        Some(Ok(keys)) => Ok(Some(super::sealed_documents::DocumentKeys::of(
            keys,
            workspace,
            collab_types::EVERY_MEMBER,
            &collab_types::app_document_id(app, resource),
        ))),
    }
}

/// `app`'s `resource` of `workspace` for the device `reader` at `now`, as
/// [`Workspaces::capture_resource`] answers it.
async fn capture_resource(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    now: SystemTime,
    maximum: usize,
) -> Result<PeerDocumentCapture, PeerDocumentReadError> {
    let keys = content_keys(peer, device, workspace, (app, resource)).await?;
    let mut opened = keys
        .as_ref()
        .map(super::sealed_documents::DocumentKeys::open);
    peer.capture_document(
        reader,
        (app, resource),
        now,
        maximum,
        opened
            .as_mut()
            .map(|open| open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>),
    )
}

/// [`Workspaces::resource_revision`] as this service answers it.
async fn resource_revision(
    peer: &StatePeer,
    device: Option<&DeviceKeys>,
    workspace: &str,
    (app, resource): (&str, &str),
    reader: [u8; 32],
    now: SystemTime,
) -> Result<Option<[u8; 32]>, String> {
    peer.check_document_read(reader, app, resource, now)?;
    if peer.checkpoint_pending(app, resource) {
        return Ok(None);
    }
    let keys = content_keys(peer, device, workspace, (app, resource))
        .await
        .map_err(|error| error.to_string())?;
    let mut opened = keys
        .as_ref()
        .map(super::sealed_documents::DocumentKeys::open);
    let mut kept = tilo_peer_state::KeptDocument::default();
    let document = peer
        .open_document(
            reader,
            (app, resource),
            now,
            usize::MAX,
            opened.as_mut().map(|open| {
                open as &mut dyn FnMut(&collab_types::SignedOp) -> Result<Vec<u8>, String>
            }),
            &mut kept,
        )
        .map_err(|error| error.to_string())?;
    Ok(Some(tilo_peer_state::committed_document_revision(
        &document.get_heads(),
    )))
}
