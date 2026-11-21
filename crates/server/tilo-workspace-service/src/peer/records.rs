//! Opening an on-demand record: a record a joining device is not replayed
//! ([`crate::store::document_sync`]) is sent to it once it asks.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx, WorkspaceOperation};
use tilo_server_protocol::WorkspaceId;

/// The recipient's policy snapshot for ordinary document frames. Attached
/// devices instead replicate the workspace as stores or key holders.
pub(super) struct DocumentRead {
    governed: bool,
    access: Option<tilo_action_policy::MemberAccess>,
    reader: [u8; 32],
}

impl DocumentRead {
    pub(super) fn of(
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
    ) -> Self {
        let governed = !runtime.attached(&peer.device_public_key)
            && (runtime.admission == crate::workspace_access::AdmissionMode::Governed
                || runtime.device_access.is_some());
        let access = runtime
            .device_access
            .as_ref()
            .and_then(|access| access.get(&peer.device_public_key))
            .cloned();
        Self {
            governed,
            access,
            reader: peer.user_public_key,
        }
    }

    /// Governance and other non-document traffic use their own admission.
    pub(super) fn allows(&self, store: &crate::store::WorkspaceStore, frame: &Envelope) -> bool {
        let Some(body) = document_policy_body(store, frame) else {
            return true;
        };
        if !self.governed {
            return true;
        }
        let Some(access) = &self.access else {
            return false;
        };
        if let Some(neighbour_wire::KeyScope::Document {
            workspace,
            app,
            document,
            readers,
        }) = key_scope(body)
        {
            return workspace.to_string() == access.workspace()
                && readers.as_deref().is_none_or(|readers| {
                    access
                        .person_id()
                        .is_some_and(|person| readers.split(',').any(|reader| reader == person))
                })
                && self.allows_route(store, access, &app, &document, None);
        }
        let Some((route, _)) = store.payloads().open_document(body) else {
            return false;
        };
        if route.cluster_id != access.workspace() {
            return false;
        }
        self.allows_route(store, access, &route.app_id, &route.document_id, route.own)
    }

    fn allows_route(
        &self,
        store: &crate::store::WorkspaceStore,
        access: &tilo_action_policy::MemberAccess,
        app: &str,
        document: &str,
        declared: Option<collab_types::OwnResource>,
    ) -> bool {
        // Key frames do not carry the record's ownership descriptor. Consult retained
        // metadata even if the original owner has since left the workspace.
        let channel = neighbour_wire::app_document_group(app, document);
        if !store.reaches_person(&channel, &self.reader) {
            return false;
        }
        let own = declared.filter(|own| own.names(document)).or_else(|| {
            collab_types::OwnResource::recognise_in(app, document, &access.policy)
        });
        if own.as_ref().is_some_and(|own| own.owner != self.reader) {
            return false;
        }
        let owner = own.map(|own| own.person());
        let mut context = tilo_action_policy::Context::at_time(tilo_clock::wall());
        if let Some(owner) = &owner {
            context = context.with_own_resource(document, owner);
        }
        let node = collab_types::document_node(access.workspace(), app, document);
        access.answer(&node, tilo_action_policy::ActionKind::Read, &context)
            == tilo_action_policy::Answer::Allow
    }
}

fn key_scope(body: &[u8]) -> Option<neighbour_wire::KeyScope> {
    use neighbour_wire::NeighbourMsg;
    if !NeighbourMsg::is_governance(body) && !NeighbourMsg::is_access_request(body) {
        return None;
    }
    let (NeighbourMsg::MlsGovernance {
        workspace: scope, ..
    }
    | NeighbourMsg::MlsAccessRequest {
        workspace: scope, ..
    }) = neighbour_wire::NeighbourMsg::decode(body).ok()?
    else {
        return None;
    };
    neighbour_wire::KeyScope::parse(&scope)
}

pub(super) fn document_policy_body<'a>(
    store: &crate::store::WorkspaceStore,
    frame: &'a Envelope,
) -> Option<&'a [u8]> {
    let Envelope::Delta { delta, .. } = frame else {
        return None;
    };
    let body = crate::coedit::retention_view(delta).map_or(delta.as_slice(), |(_, _, body)| body);
    let document = store.payloads().is_document(body);
    let scoped_keys = || {
        matches!(
            key_scope(body),
            Some(neighbour_wire::KeyScope::Document { .. })
        )
    };
    (document || scoped_keys()).then_some(body)
}

/// Handle `OpenDocument`: under the admission a replay has, mark the record
/// held by the device, so its live changes reach it from now on, then answer
/// it in `answer` with everything retained of the record and, after it,
/// [`Envelope::DocumentOpened`]; another person's own record is answered as
/// not this device's, with nothing of it. The answer is written as a replay is: no live
/// frame goes between its frames ([`super::answered_as_replay`]).
/// A chain's head ([`neighbour_wire::chain_head`]) sends the chain's newest
/// part, and the device follows the chain's heads from then on.
/// Marking first means no change committed meanwhile is missed; one that is
/// both sent and retained arrives twice, which a record takes as once.
pub(super) async fn handle_open_document(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    envelope: Envelope,
    answer: &mut Vec<Envelope>,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::OpenDocument {
        workspace_id,
        channel: requested,
        own,
    } = envelope
    else {
        return Ok(None);
    };
    let opened = crate::timing::start();
    let refused = |refusal: tilo_server_protocol::refusal::Refusal| {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            refusal,
        }))
    };
    // A record opened before the subscription goes through once the host
    // says the workspace is ready.
    if !subscribed.contains(&workspace_id) {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
    }
    // A browser environment's device opens its own record and no other.
    if session
        .environment(&peer.device_public_key)
        .is_some_and(|scope| !scope.reaches(Some(requested)))
    {
        return refused(crate::workspace_access::DenyReason::OutOfEnvironmentScope.refusal());
    }
    let locking = crate::timing::start();
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
    };
    crate::timing::event("document.open.lock", locking, 0);
    // A chain's head names whichever part is the chain's newest: that part
    // is what is sent.
    let heading = crate::timing::start();
    let head = {
        let store = ws.lock().await.store.clone();
        tokio::task::spawn_blocking(move || store.chain_head_channel(&requested))
            .await
            .map_err(|error| PeerError::Protocol(format!("record read: {error}")))??
    };
    crate::timing::event("document.open.head", heading, u64::from(head.is_some()));
    let admitting = crate::timing::start();
    let (store, governed) = {
        let mut runtime = crate::workspace_io::fresh_membership(&ws).await?;
        if !session.admits(&runtime, peer, WorkspaceOperation::State) {
            return refused(tilo_server_protocol::refusal::Refusal::NotAMember(
                "device not in workspace membership".into(),
            ));
        }
        // Another person's own record is theirs alone: this device is told so,
        // and is sent nothing of it. A host the workspace is attached to keeps
        // all of the workspace.
        // The record may be one nothing was written of yet: what the device
        // says it is counts when it names the requested opened.
        let read = head.unwrap_or(requested);
        let named_owner = own
            .as_ref()
            .filter(|own| own.names(&requested))
            .map(|own| own.record.owner);
        if !runtime.attached(&peer.device_public_key)
            && (!runtime.store.reaches_person(&read, &peer.user_public_key)
                || named_owner.is_some_and(|owner| owner != peer.user_public_key))
        {
            return Ok(Some(Envelope::DocumentOpened {
                workspace_id,
                channel: requested,
                answer: tilo_server_protocol::DocumentAnswer::NotYours,
            }));
        }
        // Its person's device opening their own record says whose it is.
        if let Some(own) = own
            .as_ref()
            .filter(|own| own.names(&requested) && own.record.owner == peer.user_public_key)
        {
            runtime.store.learn_own(&own.app_id, &own.record);
        }
        let device = &peer.device_public_key;
        runtime.hold_document(device, session.outbound, requested);
        if let Some(head) = head {
            runtime.hold_document(device, session.outbound, head);
        }
        (
            runtime.store.clone(),
            runtime
                .workspace_authority
                .as_ref()
                .and_then(crate::workspaces::WorkspaceAuthority::workspaces),
        )
    };
    crate::timing::event("document.open.admission", admitting, 0);
    // Sent whether or not the device holds the head already: an answer of no
    // frames says the chain has no part at all, never "you have it", so a
    // device whose copy has not opened yet never takes the chain as empty.
    let read = head.unwrap_or(requested);
    let frames = frames_of(store, governed, workspace_id, read).await?;
    if !still_readable(&ws, peer, session, (requested, head), &frames).await? {
        return Ok(Some(Envelope::DocumentOpened {
            workspace_id,
            channel: requested,
            answer: tilo_server_protocol::DocumentAnswer::NotYours,
        }));
    }
    let sent = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    crate::timing::event("document.open.total", opened, u64::from(sent));
    answer.extend(frames);
    Ok(Some(Envelope::DocumentOpened {
        workspace_id,
        channel: requested,
        answer: tilo_server_protocol::DocumentAnswer::Sent { frames: sent },
    }))
}

/// Apply the current policy after loading the retained frames.
async fn still_readable(
    workspace: &crate::workspace::SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    (requested, head): ([u8; 32], Option<[u8; 32]>),
    frames: &[Envelope],
) -> Result<bool, PeerError> {
    let mut runtime = crate::workspace_io::fresh_membership(workspace).await?;
    let policy = DocumentRead::of(&runtime, peer);
    let allowed = frames
        .iter()
        .all(|frame| policy.allows(&runtime.store, frame));
    if !allowed {
        let device = &peer.device_public_key;
        runtime.unhold_document(device, session.outbound, &requested);
        if let Some(head) = head {
            runtime.unhold_document(device, session.outbound, &head);
        }
    }
    drop(runtime);
    Ok(allowed)
}

/// Everything `store` retains of the document on `read`, beside the writes
/// `governed`'s link keeps that the store's copy lacks: a linked member's
/// write whose copy to the server has not landed yet is kept by the
/// workspace's link, and sent from there, so a device never hears the document
/// is absent while the workspace's host keeps it.
async fn frames_of(
    store: crate::store::WorkspaceStore,
    governed: Option<crate::workspaces::Workspaces>,
    workspace_id: WorkspaceId,
    read: [u8; 32],
) -> Result<Vec<Envelope>, PeerError> {
    let reading = crate::timing::start();
    let frames = tokio::task::spawn_blocking(move || {
        let mut frames = store.record_frames(&read)?;
        if let Some(governed) = governed {
            match governed.link_frames_lacking(workspace_id, read, &frames) {
                Ok(lacking) => frames.extend(lacking),
                Err(error) => {
                    return Err(PeerError::Protocol(format!(
                        "the workspace link's history of the document: {error}"
                    )));
                }
            }
        }
        Ok::<_, PeerError>(frames)
    })
    .await
    .map_err(|error| PeerError::Protocol(format!("document read: {error}")))??;
    let sent = u64::try_from(frames.len()).unwrap_or(u64::MAX);
    crate::timing::event("document.open.frames", reading, sent);
    Ok(frames)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::payloads::{NeighbourPayloads, Route, ShellPayloads as _};
    use collab_types::{Ed25519PublicKey, OwnResource, SignedOp};
    use tilo_action_policy::{Acting, Actor, MemberAccess, MemberId, Role, WorkspacePolicy};

    #[test]
    fn a_deleted_members_retained_record_keeps_its_private_key_replay() {
        const APP: &str = "so.tilo.tube";
        let root = tempfile::tempdir().unwrap();
        let workspace = WorkspaceId::from_u128(1);
        let store = crate::store::WorkspaceStore::open(root.path(), workspace, false).unwrap();
        let reader = [1; 32];
        let workspace_owner = [2; 32];
        let own = OwnResource::new("tube.watch-history", reader).unwrap();
        let record = own.document_id();
        let route = Route {
            app_id: APP.to_owned(),
            cluster_id: workspace.to_string(),
            document_id: record.clone(),
            own: Some(own),
            readers: None,
        };
        let device = ed25519_dalek::SigningKey::from_bytes(&[3; 32]);
        let source = device.verifying_key().to_bytes();
        let checkpoint = |channel, body| Envelope::Delta {
            workspace_id: workspace,
            author_device_public_key: source,
            authored_at_ns: 1,
            delta: crate::coedit::CoeditPayload::Checkpoint { channel, body }
                .encode()
                .unwrap(),
            signature: [0; 64],
        };
        let mut operation = SignedOp::new_unsigned(
            Vec::new(),
            collab_types::document_artifact_id(APP, &route.cluster_id, &record),
            Ed25519PublicKey::from_bytes(source),
            collab_types::document_attestation_id(APP, &route.cluster_id, &record),
            collab_types::OpKind::Change,
            tilo_clock::wall(),
            1,
        );
        collab_types::sign_op(&mut operation, &device).unwrap();
        let channel = neighbour_wire::app_document_group(APP, &record);
        let document = checkpoint(
            channel,
            NeighbourPayloads.document_body(&source, &route, operation).unwrap(),
        );
        store
            .retain_checkpoint(&channel, &crate::transport::encode(&document).unwrap())
            .unwrap();
        drop(store);

        // The key message itself contains no record-owner descriptor.
        let scope = neighbour_wire::KeyScope::document(workspace, APP, &record, None).unwrap();
        let message = neighbour_wire::NeighbourMsg::MlsAccessRequest {
            workspace: scope.to_string(),
            request: Box::new(
                collab_types::MlsAccessRequestV1::signed(scope.group(), vec![1], 1, &device)
                    .unwrap(),
            ),
            delivery: 0,
        };
        let (key_channel, _) = neighbour_wire::retention(&message, false).unwrap();
        let keys = checkpoint(key_channel, postcard::to_stdvec(&message).unwrap());
        let store = crate::store::WorkspaceStore::open(root.path(), workspace, false).unwrap();
        let mut policy = WorkspacePolicy::new(workspace.to_string(), hex::encode(workspace_owner));
        policy.members.insert(hex::encode(reader), Role::Viewer);
        let access = |policy, reader| {
            let actor = Acting::from(Actor::Member(MemberId::person(hex::encode(reader))));
            DocumentRead {
                governed: true,
                access: Some(MemberAccess::new(policy, actor)),
                reader,
            }
        };
        assert!(access(policy.clone(), reader).allows(&store, &keys));
        policy.members.remove(&hex::encode(reader));
        let keeper = access(policy, workspace_owner);
        assert!(!keeper.allows(&store, &keys));
        assert!(!keeper.allows(&store, &document));
    }
}
