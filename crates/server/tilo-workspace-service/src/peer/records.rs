//! Opening an on-demand record: a record a joining device is not replayed
//! ([`crate::store::document_sync`]) is sent to it once it asks.

use super::{AuthenticatedPeer, Envelope, PeerError, SessionCtx, WorkspaceOperation};
use tilo_server_protocol::WorkspaceId;

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
        channel,
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
        .is_some_and(|scope| !scope.reaches(Some(channel)))
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
        tokio::task::spawn_blocking(move || store.chain_head_channel(&channel))
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
        // says it is counts when it names the channel opened.
        let read = head.unwrap_or(channel);
        let named_owner = own
            .as_ref()
            .filter(|own| own.names(&channel))
            .map(|own| own.record.owner);
        if !runtime.attached(&peer.device_public_key)
            && (!runtime.store.reaches_person(&read, &peer.user_public_key)
                || named_owner.is_some_and(|owner| owner != peer.user_public_key))
        {
            return Ok(Some(Envelope::DocumentOpened {
                workspace_id,
                channel,
                answer: tilo_server_protocol::DocumentAnswer::NotYours,
            }));
        }
        // Its person's device opening their own record says whose it is.
        if let Some(own) = own
            .as_ref()
            .filter(|own| own.names(&channel) && own.record.owner == peer.user_public_key)
        {
            runtime.store.learn_own(&own.app_id, &own.record);
        }
        let device = &peer.device_public_key;
        runtime.hold_document(device, channel);
        if let Some(head) = head {
            runtime.hold_document(device, head);
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
    let read = head.unwrap_or(channel);
    let reading = crate::timing::start();
    let frames = tokio::task::spawn_blocking(move || {
        let mut frames = store.record_frames(&read)?;
        // A linked member's write whose copy to the server has not landed yet
        // is kept by the workspace's link: sent from there, so the page never
        // hears the record is absent while the workspace's host keeps it.
        if let Some(governed) = governed {
            match governed.link_frames_lacking(workspace_id, read, &frames) {
                Ok(lacking) => frames.extend(lacking),
                Err(error) => {
                    return Err(PeerError::Protocol(format!(
                        "the workspace link's history of the record: {error}"
                    )));
                }
            }
        }
        Ok::<_, PeerError>(frames)
    })
    .await
    .map_err(|error| PeerError::Protocol(format!("record read: {error}")))??;
    let sent = u32::try_from(frames.len()).unwrap_or(u32::MAX);
    crate::timing::event("document.open.frames", reading, u64::from(sent));
    crate::timing::event("document.open.total", opened, u64::from(sent));
    answer.extend(frames);
    Ok(Some(Envelope::DocumentOpened {
        workspace_id,
        channel,
        answer: tilo_server_protocol::DocumentAnswer::Sent { frames: sent },
    }))
}
