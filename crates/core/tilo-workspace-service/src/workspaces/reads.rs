//! The resources of the workspaces a service holds, read for a device.
//!
//! The service's own copy of a workspace is the one its apps' Opens and the session's
//! own reads answer with: what its host keeps of a document whole, sealed writes opened with
//! the content keys the device the service is on holds. Every read is decided at the resource's
//! node as a member's read is ([`tilo_peer_state::StatePeer::check_document_read`]), for the
//! device whose certificate asks.

#[cfg(feature = "apps")]
use tilo_peer_state::PeerDocumentReadError;
use tilo_peer_state::StatePeer;
#[cfg(feature = "apps")]
use tilo_user_identity::DeviceCert;

use super::{DeviceKeys, Workspaces};
use crate::documents::OpenedWith;
use tilo_actions::remote::{ListedResource, ResourceAddress};
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
    pub async fn own_read(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        since: &[[u8; 32]],
        maximum: usize,
    ) -> Result<DocumentRead, String> {
        if let Some(read) = self.stored_read(workspace, (app, resource), since, maximum)? {
            return Ok(read);
        }
        let (peer, device) = self
            .replica(workspace)
            .ok_or("this service does not hold the workspace")?;
        let reader = self.own_reader();
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

    /// `app`'s record `resource` of `workspace` as this service's person reads it from the
    /// workspace's store, as [`Self::own_read`] answers: `None` for a record the store holds none
    /// of, which is a document of the workspace's own.
    ///
    /// # Errors
    /// The person may not read the record, its document does not read, or it exceeds `maximum`.
    fn stored_read(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        since: &[[u8; 32]],
        maximum: usize,
    ) -> Result<Option<DocumentRead>, String> {
        let Some(held) = self.inner.readings.held(workspace) else {
            return Ok(None);
        };
        let reader = hex::encode(self.device_cert().user_public_key);
        let may_read = tilo_resource_store::wire::reads_as(&reader);
        let mut store = held.store.lock_recover();
        stored_read_in(
            &mut store,
            &self.inner.host.surfaces,
            (app, resource),
            &may_read,
            since,
            maximum,
        )
    }

    /// The records `workspace`'s store holds, by their app, id and kind, for its listings.
    fn stored_routes(&self, workspace: &str) -> Vec<ListedResource> {
        let Some(held) = self.inner.readings.held(workspace) else {
            return Vec::new();
        };
        let store = held.store.lock_recover();
        stored_routes_in(&store, &self.inner.host.surfaces, workspace)
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
        if self
            .stored_read(workspace, (app, resource), &[], usize::MAX)
            .is_ok_and(|read| read.is_some())
        {
            return true;
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
    pub(crate) fn own_listing(&self, workspace: &str) -> Result<Vec<ListedResource>, String> {
        use tilo_actions::remote::{KEY_GRANTS, PRESENCE};
        let mut listed = self.own_documents(workspace)?;
        let document = |app: &str, record: String| {
            ListedResource::document(ResourceAddress {
                workspace: workspace.to_owned(),
                app: app.to_owned(),
                record,
            })
        };
        listed.push(document(PRESENCE, String::new()));
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        listed.extend(held.replica.read(|peer| {
            peer.governance()
                .workspace_grants()
                .into_iter()
                .map(|grant| document(KEY_GRANTS, grant.id))
                .collect::<Vec<_>>()
        }));
        Ok(listed)
    }

    /// Every document `workspace` holds here, by its app and route: what is stored of it, which
    /// a copy of the workspace takes. The documents of the workspace's store are listed by the
    /// records they hold, each with its kind.
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    pub fn own_documents(&self, workspace: &str) -> Result<Vec<ListedResource>, String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        let mut listed: Vec<ListedResource> = held
            .replica
            .read(crate::documents::listing)
            .into_iter()
            .filter(|(_, document)| !collab_types::is_store_document(document))
            .map(|(app, record)| {
                ListedResource::document(ResourceAddress {
                    workspace: workspace.to_owned(),
                    app,
                    record,
                })
            })
            .collect();
        listed.extend(self.stored_routes(workspace));
        Ok(listed)
    }

    /// Every change of `workspace`'s members its governance names, oldest first, the actions
    /// its bases kept among them ([`tilo_peer_state::workspace::base::named_changes`]).
    ///
    /// # Errors
    ///
    /// This service holds no such workspace.
    pub(crate) fn member_changes(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::workspace_documents::MemberChange>, String> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or("this service does not hold the workspace")?;
        Ok(held.replica.read(|peer| {
            peer.admitted_workspace()
                .map(tilo_peer_state::workspace::base::named_changes)
                .unwrap_or_default()
                .into_iter()
                .map(Into::into)
                .collect()
        }))
    }

    /// The workspaces this service holds, by their ids.
    pub fn held_workspaces(&self) -> Vec<String> {
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
    pub fn seals_resources(&self, workspace: &str) -> bool {
        self.inner
            .readings
            .held(workspace)
            .is_some_and(|held| held.device.is_some())
    }

    /// Whether `workspace`'s store holds `app`'s record `resource`, whoever may read it.
    pub fn holds_resource(&self, workspace: &str, app: &str, resource: &str) -> bool {
        self.inner.readings.held(workspace).is_some_and(|held| {
            let store = held.store.lock_recover();
            self.inner
                .host
                .surfaces
                .kinds_of(app)
                .iter()
                .any(|kind| store.index().get(kind, resource).is_some())
        })
    }

    /// The revision `app`'s record `resource` of `workspace` stands at for the person `reader`
    /// (their key, hex), handing nothing out: `None` for a record the store holds none of that
    /// they may read.
    ///
    /// # Errors
    /// The service holds no copy of the workspace.
    #[cfg(feature = "apps")]
    pub fn resource_revision(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: &str,
    ) -> Result<Option<[u8; 32]>, String> {
        Ok(self
            .stored_capture(workspace, (app, resource), reader, None)
            .map_err(|error| error.to_string())?
            .map(|capture| capture.revision))
    }

    /// Whether `certificate`'s device may read `app`'s `resource` of `workspace` now: a device the
    /// workspace certifies, a current member, and the resource's Read permission as its governance
    /// grants it.
    #[cfg(feature = "apps")]
    pub fn authorizes_resource_read(
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

    /// `app`'s record `resource` of `workspace` as the person `reader` (their key, hex) reads it
    /// now from the workspace's store: at most `maximum` bytes of it, with all of its history.
    /// `None` for a record the store holds none of that they may read.
    ///
    /// # Errors
    /// The service holds no copy, the record's document does not read, or it exceeds `maximum`.
    #[cfg(feature = "apps")]
    pub fn capture_resource(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: &str,
        maximum: usize,
    ) -> Result<Option<StoredCapture>, PeerDocumentReadError> {
        self.stored_capture(workspace, (app, resource), reader, Some(maximum))
    }

    /// [`Self::capture_resource`], saving the record only when `maximum` asks for its bytes.
    #[cfg(feature = "apps")]
    fn stored_capture(
        &self,
        workspace: &str,
        (app, resource): (&str, &str),
        reader: &str,
        maximum: Option<usize>,
    ) -> Result<Option<StoredCapture>, PeerDocumentReadError> {
        let held = self
            .inner
            .readings
            .held(workspace)
            .ok_or(PeerDocumentReadError::Missing)?;
        let may_read = tilo_resource_store::wire::reads_as(reader);
        let mut store = held.store.lock_recover();
        stored_capture_in(
            &mut store,
            &self.inner.host.surfaces,
            (app, resource),
            &may_read,
            maximum,
        )
    }

    /// The replica of `workspace` and the content keys its device holds, taken in a moment: the
    /// host stays free while a read opens a sealed history.
    fn replica(&self, workspace: &str) -> Option<(StatePeer, Option<DeviceKeys>)> {
        let held = self.inner.readings.held(workspace)?;
        Some((held.replica.read(StatePeer::clone), held.device))
    }
}

/// [`Workspaces::stored_read`] of `app`'s record `resource` as `store` holds it, the heads `since`
/// having been read already: what the store is taken for, and no longer.
fn stored_read_in(
    store: &mut tilo_resource_store::Store,
    surfaces: &collab_types::app_surface::Surfaces,
    (app, resource): (&str, &str),
    may_read: &dyn Fn(&str) -> bool,
    since: &[[u8; 32]],
    maximum: usize,
) -> Result<Option<DocumentRead>, String> {
    let Some(kind) = surfaces
        .kinds_of(app)
        .into_iter()
        .find(|kind| store.index().get(kind, resource).is_some())
    else {
        return Ok(None);
    };
    if store.get(&kind, resource, may_read).is_none() {
        return Err(format!(
            "this device's person may not read {resource} of {app}"
        ));
    }
    let Some(document) = store
        .content_document(&kind, resource)
        .map_err(|error| error.to_string())?
    else {
        return Ok(Some(DocumentRead::Absent));
    };
    let at = document.get_heads();
    let revision = record_revision(&at);
    let heads: Vec<[u8; 32]> = at.iter().map(|head| head.0).collect();
    let fits = |bytes: Vec<u8>| {
        if bytes.len() > maximum {
            Err(format!("{resource} of {app} is larger than one read takes"))
        } else {
            Ok(bytes)
        }
    };
    if !since.is_empty() {
        let (mut now, mut held) = (heads.clone(), since.to_vec());
        now.sort_unstable();
        held.sort_unstable();
        if now == held {
            return Ok(Some(DocumentRead::Unchanged(revision)));
        }
        let held: Vec<automerge::ChangeHash> = since
            .iter()
            .map(|head| automerge::ChangeHash(*head))
            .collect();
        if held.iter().all(|head| document.has_change(head)) {
            let bytes = fits(document.save_after(&held))?;
            return Ok(Some(DocumentRead::Changes {
                revision,
                heads,
                bytes,
            }));
        }
    }
    let bytes = fits(document.save())?;
    Ok(Some(DocumentRead::Whole {
        revision,
        heads,
        bytes,
        hint: None,
    }))
}

/// [`Workspaces::stored_routes`] of the records `store` holds of the kinds `surfaces` declare.
fn stored_routes_in(
    store: &tilo_resource_store::Store,
    surfaces: &collab_types::app_surface::Surfaces,
    workspace: &str,
) -> Vec<ListedResource> {
    let index = store.index();
    let mut routes = Vec::new();
    for (app, kind) in surfaces.kinds() {
        for id in index.ids_of(&kind) {
            routes.push(ListedResource {
                address: ResourceAddress {
                    workspace: workspace.to_owned(),
                    app: app.clone(),
                    record: id.to_owned(),
                },
                kind: Some(kind.clone()),
            });
        }
    }
    routes
}

/// [`Workspaces::stored_capture`] of `app`'s record `resource` as `store` holds it, the reader's
/// admission `may_read` having been read already: what the store is taken for, and no longer.
#[cfg(feature = "apps")]
fn stored_capture_in(
    store: &mut tilo_resource_store::Store,
    surfaces: &collab_types::app_surface::Surfaces,
    (app, resource): (&str, &str),
    may_read: &dyn Fn(&str) -> bool,
    maximum: Option<usize>,
) -> Result<Option<StoredCapture>, PeerDocumentReadError> {
    let Some(kind) = surfaces
        .kinds_of(app)
        .into_iter()
        .find(|kind| store.get(kind, resource, may_read).is_some())
    else {
        return Ok(None);
    };
    let Some(document) = store
        .content_document(&kind, resource)
        .map_err(|error| PeerDocumentReadError::Opening(error.to_string()))?
    else {
        return Ok(None);
    };
    let heads = document.get_heads();
    let bytes = match maximum {
        Some(maximum) => {
            let bytes = document.save();
            if bytes.len() > maximum {
                return Err(PeerDocumentReadError::Capacity);
            }
            bytes
        }
        None => Vec::new(),
    };
    Ok(Some(StoredCapture {
        revision: record_revision(&heads),
        heads,
        bytes,
    }))
}

/// A record of a workspace's store as one read of it captured it.
#[cfg(feature = "apps")]
pub struct StoredCapture {
    /// What the record stands at: equal revisions at two moments mean it took no write between.
    pub revision: [u8; 32],
    /// Its heads.
    pub heads: Vec<automerge::ChangeHash>,
    /// The record saved with all of its history.
    pub bytes: Vec<u8>,
}

/// The revision a record whose document stands at `heads` is at: any write to it moves it.
pub(crate) fn record_revision(heads: &[automerge::ChangeHash]) -> [u8; 32] {
    let mut heads = heads.to_vec();
    heads.sort();
    let mut hasher = blake3::Hasher::new_derive_key("tilo record revision v1");
    for head in &heads {
        hasher.update(&head.0);
    }
    *hasher.finalize().as_bytes()
}

/// Sealed resources as this service's own device last opened them ([`crate::documents::Opened`]).
pub(super) use crate::documents::Opened as OwnOpened;
