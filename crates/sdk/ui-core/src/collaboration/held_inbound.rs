//! What waited on a route's keys or verdict, delivered once the route
//! changes: held edits go out, held ops are decided again, and what a full
//! hold let go of is paged back from history.

use collab_types::SignedOp;
use collab_types::inbound::OpKey;
use runtime_collab::OpAdmission;

use super::{
    Arrived, CollabEvent, InboundBridge, LocalSdkDocumentSigner, RouteLabel, SdkRoute,
    UndeliveredDocument, complete_document_event, send_document_frame, signed_document_event,
};

/// Report `route`'s access change to the app, then send the edits and
/// deliver the ciphertext that waited on its keys. `false` once the app
/// side is gone.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
pub(super) async fn deliver_waiting(
    route: &SdkRoute,
    signer: &mut LocalSdkDocumentSigner,
    inbound_tx: &InboundBridge,
) -> bool {
    let (state, outbound, inbound) = {
        let Ok(mut keys) = route.encryption.lock() else {
            return true;
        };
        (
            keys.take_state_change(),
            keys.take_held_outbound(),
            keys.take_held_inbound(),
        )
    };
    if let Some(state) = state
        && !inbound_tx.send_durable(CollabEvent::ChannelAccess {
            route: route.spec.clone(),
            state,
        })
    {
        return false;
    }
    for (frame, claims) in outbound {
        if send_document_frame(route, signer, frame, &claims, &inbound_tx.state)
            .await
            .is_err()
        {
            tracing::warn!("collab: an edit that waited for channel access was not sent");
        }
    }
    for arrived in inbound {
        if !deliver_decided(route, arrived, inbound_tx).await {
            return false;
        }
    }
    deliver_owed(route, inbound_tx).await
}

/// Decide an op that waited, again: deliver it, hold it again, or say why
/// it is refused. `false` once the app side is gone.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn deliver_decided(route: &SdkRoute, arrived: Arrived, inbound_tx: &InboundBridge) -> bool {
    let op = OpKey::of(&arrived.envelope);
    let event = match signed_document_event(arrived, &route.encryption) {
        Ok(event) => event,
        Err(UndeliveredDocument::Held) => return true,
        Err(UndeliveredDocument::Refused(error)) => {
            let label = RouteLabel::of(&route.spec);
            tracing::warn!(
                %error,
                app = %label.app,
                workspace = %label.workspace,
                record = %label.record,
                route = %label.channel,
                %op,
                "collab: refused a document op that waited"
            );
            return true;
        }
    };
    let Some(complete) = complete_document_event(event, &inbound_tx.reassembly.live) else {
        return true;
    };
    inbound_tx.send_document(complete).await
}

/// Page history back for the ops a full hold let go of, and deliver each
/// once. What history no longer has is said, by op, and forgotten.
#[allow(
    clippy::future_not_send,
    reason = "capnp-rpc routes are !Send and this runs only on the shovel LocalSet"
)]
async fn deliver_owed(route: &SdkRoute, inbound_tx: &InboundBridge) -> bool {
    let pages = route
        .encryption
        .lock()
        .map(|keys| keys.owed_pages())
        .unwrap_or_default();
    for (artifact, mut before_seq) in pages {
        loop {
            let Ok(batch) = route
                .channel
                .fetch_scrollback_for(
                    artifact.0.as_bytes(),
                    before_seq,
                    u32::from(crate::collaboration::HISTORY_PAGE_SIZE),
                )
                .await
            else {
                // Still owed: paged again after the route's next change.
                break;
            };
            let ops: Vec<SignedOp> = batch
                .ops
                .iter()
                .filter_map(|bytes| postcard::from_bytes(bytes).ok())
                .collect();
            let oldest = ops.iter().map(|op| op.seq).min();
            for op in ops {
                let owed = route.encryption.lock().is_ok_and(|mut keys| {
                    let owed = keys.owes(&op);
                    keys.settle(&op);
                    owed
                });
                if !owed {
                    continue;
                }
                let arrived = Arrived {
                    from_user_id: hex::encode(op.author.as_bytes()),
                    envelope: op,
                    // The local broker pages what it admitted.
                    admission: OpAdmission::CleartextAdmitted,
                };
                if !deliver_decided(route, arrived, inbound_tx).await {
                    return false;
                }
            }
            let still_owed = route
                .encryption
                .lock()
                .is_ok_and(|keys| keys.owed_pages().iter().any(|(owed, _)| *owed == artifact));
            match oldest {
                _ if !still_owed => break,
                Some(seq) if !batch.done => before_seq = seq,
                _ => {
                    let given_up = route
                        .encryption
                        .lock()
                        .map(|mut keys| keys.give_up_owed(artifact))
                        .unwrap_or_default();
                    for op in given_up {
                        tracing::warn!(%op, "collab: history has no op the inbound hold let go of");
                    }
                    break;
                }
            }
        }
    }
    true
}
