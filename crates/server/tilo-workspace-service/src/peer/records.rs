//! Opening a record a session is not replayed: the host sends what its store
//! keeps of it, then says it did.
//!
//! A workspace's records reach a device over its link to the workspace. A
//! client that still reads them through its session — an agent's workspace
//! actions, and the desktop's co-edit session — is answered here from the same
//! store the link keeps, so what it reads is what the workspace holds. This
//! goes with those clients' move onto the device link.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx, WorkspaceOperation};
use tilo_peer_state::direct::DirectSync;
use tilo_server_protocol::{DocumentAnswer, WorkspaceId};

/// Who may read a record of a workspace: a device the workspace's host is attached
/// to keeps all of it, and any other is sent a record the workspace's policy lets
/// its person read.
struct RecordRead {
    /// This device belongs to a host the workspace is attached to: it keeps all of
    /// the workspace, whether or not its person is a member.
    attached: bool,
    /// This workspace's admission is the one that decides who reads its records.
    governed: bool,
    access: Option<tilo_action_policy::MemberAccess>,
    reader: [u8; 32],
}

impl RecordRead {
    fn of(runtime: &crate::workspace::WorkspaceRuntime, peer: &AuthenticatedPeer) -> Self {
        let attached = runtime.attached(&peer.device_public_key);
        Self {
            attached,
            governed: !attached
                && (runtime.admission == crate::workspace_access::AdmissionMode::Governed
                    || runtime.device_access.is_some()),
            access: runtime
                .device_access
                .as_ref()
                .and_then(|access| access.get(&peer.device_public_key))
                .cloned(),
            reader: peer.user_public_key,
        }
    }

    /// Whether `frame`, a record's write, may be sent to this device: the record a
    /// set of people reads is theirs alone, and the policy lets its person read it.
    fn allows(&self, frame: &neighbour_wire::NeighbourMsg) -> bool {
        let neighbour_wire::NeighbourMsg::AppDocument {
            app_id,
            cluster_id,
            document_id,
            readers,
            ..
        } = frame
        else {
            return true;
        };
        if !self.governed {
            return true;
        }
        let Some(access) = &self.access else {
            return false;
        };
        if cluster_id.as_str() != access.workspace() {
            return false;
        }
        let named = readers.as_deref().is_none_or(|named| {
            collab_types::Readers::parse(named)
                .is_some_and(|readers| readers.contains(&self.reader))
        });
        if !named {
            return false;
        }
        let node = collab_types::document_node(access.workspace(), app_id, document_id);
        let context = tilo_action_policy::Context::at_time(tilo_clock::wall());
        access.answer(&node, tilo_action_policy::ActionKind::Read, &context)
            == tilo_action_policy::Answer::Allow
    }
}

/// Handle `OpenDocument`: answer with everything this device's store keeps of
/// the record on `channel`, one frame each, and then [`Envelope::DocumentOpened`]
/// saying how many; another person's own record is answered as not this
/// device's, with nothing of it. A chain's head ([`neighbour_wire::chain_head`])
/// names whichever part is the chain's newest: that part is what is sent.
pub(super) async fn handle_open_document(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
    envelope: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::OpenDocument {
        workspace_id,
        channel: requested,
    } = envelope
    else {
        return Ok(None);
    };
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
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
    };
    // The workspace's one admission for a device reading its records, as a
    // link's other end is admitted before it is sent any, and what this device
    // may read of them.
    let runtime = crate::workspace_io::fresh_membership(&ws).await?;
    if !session.admits(&runtime, peer, WorkspaceOperation::State) {
        return refused(tilo_server_protocol::refusal::Refusal::NotAMember(
            "device not in workspace membership".into(),
        ));
    }
    let read_policy = RecordRead::of(&runtime, peer);
    drop(runtime);
    let Some(link) = session
        .admin
        .and_then(|admin| admin.direct().link(workspace_id))
    else {
        return refused(tilo_server_protocol::refusal::Refusal::NotReady);
    };
    // A chain's head names a part and holds nothing of its own: the chain's
    // newest part is what is sent, and the device follows the chain from then on
    // (`Workspaces::watch_record`). Reading a store's frames touches the disk,
    // so it reads on the blocking pool.
    let kept = |channel: [u8; 32]| {
        let link = link.clone();
        async move {
            tokio::task::spawn_blocking(move || link.kept_record(channel))
                .await
                .map_err(|error| PeerError::Protocol(format!("record read: {error}")))?
                .map_err(|error| PeerError::Protocol(format!("record read: {error}")))
        }
    };
    let mut read = requested;
    let mut frames = kept(requested).await?.unwrap_or_default();
    if frames.is_empty()
        && let Some(part) = chain_head_part(&link, requested)
    {
        read = part;
        frames = kept(part).await?.unwrap_or_default();
    }
    // A document a set of people reads is theirs alone: a device the record's
    // readers do not name, or the workspace's policy no longer lets read it,
    // is told so and sent nothing of it.
    if !read_policy.attached && !frames.iter().all(|frame| read_policy.allows(frame)) {
        return Ok(Some(Envelope::DocumentOpened {
            workspace_id,
            channel: requested,
            answer: DocumentAnswer::NotYours,
        }));
    }
    // Watched before its frames go, as a replay's are: a write committed
    // meanwhile reaches the connection too, and one sent twice a record takes
    // as once. A device the record is not its own is never sent a write of it
    // (`Workspaces::deliver_record_write` asks again).
    if let Some(admin) = session.admin {
        for channel in [requested, read] {
            admin.watch_record(
                workspace_id,
                peer.device_public_key,
                session.outbound,
                session.disconnect,
                channel,
            );
        }
    }
    let mut sent = 0_u32;
    for message in &frames {
        let Some(delta) = delta_of(
            workspace_id,
            tilo_peer_state::store::author_of(message).unwrap_or_default(),
            message,
        ) else {
            continue;
        };
        session
            .outbound
            .send(delta)
            .await
            .map_err(|_| PeerError::Protocol("the session's writer ended".into()))?;
        sent = sent.saturating_add(1);
    }
    Ok(Some(Envelope::DocumentOpened {
        workspace_id,
        channel: requested,
        answer: DocumentAnswer::Sent { frames: sent },
    }))
}

/// The channel of the newest part of the chain whose head `requested` names,
/// as this device holds the workspace's records, or `None` when `requested` is
/// no chain's head or no part of it is here. A part is numbered within its
/// chain, so the newest is the greatest number kept.
fn chain_head_part(link: &DirectSync, requested: [u8; 32]) -> Option<[u8; 32]> {
    let mut newest: Option<(u64, [u8; 32])> = None;
    for (app, record) in link.read(crate::documents::listing) {
        let neighbour_wire::DocumentSync::Part { base, index } =
            neighbour_wire::document_sync(&record)
        else {
            continue;
        };
        if neighbour_wire::app_document_group(&app, &neighbour_wire::chain_head(base)) != requested
        {
            continue;
        }
        let channel = neighbour_wire::app_document_group(&app, &record);
        if newest.is_none_or(|(newest, _)| index > newest) {
            newest = Some((index, channel));
        }
    }
    newest.map(|(_, channel)| channel)
}

/// `message`, `author`'s frame of `workspace_id`, as a session carries it: the
/// co-edit payload a client applies, which holds the frame's signed body as it
/// is. A frame the workspace keeps as a checkpoint is carried as one. `None` for
/// a frame that is no workspace's, or one that does not encode.
pub(crate) fn delta_of(
    workspace_id: WorkspaceId,
    author: [u8; 32],
    message: &neighbour_wire::NeighbourMsg,
) -> Option<Envelope> {
    let (channel, retention) = neighbour_wire::retention(message, false)?;
    let body = postcard::to_allocvec(message).ok()?;
    let payload = if retention == neighbour_wire::Retention::Checkpoint {
        crate::coedit::CoeditPayload::Checkpoint { channel, body }
    } else {
        crate::coedit::CoeditPayload::RetainedDelta { channel, body }
    };
    Some(Envelope::Delta {
        workspace_id,
        author_device_public_key: author,
        authored_at_ns: tilo_clock::unix_now_ns(),
        delta: payload.encode().ok()?,
        signature: [0; 64],
    })
}
