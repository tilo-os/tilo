//! The two inbound dispatchers: [`handle_envelope`] for the v1 state lane
//! ([`Envelope`]) and [`handle_private`] for the v2 private lane
//! ([`EnvelopeV2`]). Each arm hands one envelope to its concern's handler.

use super::*;

/// Handle one successfully-decoded inbound envelope: dispatch to
/// [`handle_envelope`], then write any replay backlog to the wire —
/// followed by the direct reply, if any. Pending on-disk backlog
/// files are cleaned up only after being written, so a write failure
/// leaves them intact for the next reconnect.
#[cfg(test)]
pub(super) async fn handle_inbound_frame<W>(
    env: Envelope,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    write: &mut W,
) -> Result<(), PeerError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let replaying = matches!(
        env,
        Envelope::Subscribe { .. } | Envelope::SubscribeFrom { .. }
    );
    let mut ticket = None;
    let reply = handle_envelope(peer, env, session, subscribed, &mut ticket).await?;
    let pending = std::sync::atomic::AtomicUsize::new(usize::from(replaying));
    let (_tx, mut private_rx) = mpsc::channel(1);
    Box::pin(finish_replay_write(
        ReplayWrite {
            ticket,
            reply,
            replaying,
        },
        peer,
        session,
        &pending,
        &mut private_rx,
        write,
    ))
    .await
}

pub(super) async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
            handle_subscribe(
                peer,
                session,
                subscribed,
                replay,
                workspace_id,
                Subscription::Whole,
            )
            .await
        }
        Envelope::SubscribeFrom {
            workspace_id,
            position,
        } => {
            handle_subscribe(
                peer,
                session,
                subscribed,
                replay,
                workspace_id,
                Subscription::From(position),
            )
            .await
        }
        Envelope::Unsubscribe { workspace_id } => {
            handle_unsubscribe(peer, session, subscribed, workspace_id).await
        }
        env @ Envelope::Delta { .. } => handle_delta(peer, session, subscribed, env).await,
        env @ Envelope::Presence { .. } => handle_presence(peer, session, subscribed, env).await,
        Envelope::PeerRelayHello { hosted_workspaces } => {
            handle_peer_relay_hello(peer, session, hosted_workspaces).await
        }
        Envelope::SyncRequest {
            workspace_id,
            have_heads,
        } => handle_sync_request(peer, session, workspace_id, have_heads).await,
        Envelope::Ping => Ok(Some(Envelope::Pong)),
        Envelope::RemoteAction {
            workspace_id,
            version,
            to_device_public_key,
            frame,
            ..
        } => Ok(handle_remote_action(
            peer,
            session,
            subscribed,
            RemoteActionRoute {
                workspace_id,
                version,
                to: to_device_public_key,
            },
            frame,
        )
        .await),
        env @ Envelope::RecordContent { .. } => {
            route_record_content(peer, session, subscribed, env).await;
            Ok(None)
        }
        env @ Envelope::Commit { .. } => handle_commit(peer, session, subscribed, env).await,
        Envelope::LiveJoin { workspace_id } => {
            handle_live_join(peer, session, subscribed, workspace_id).await
        }
        Envelope::LiveLeave { workspace_id } => {
            handle_live_leave(peer, session, workspace_id).await
        }
        Envelope::Live {
            workspace_id,
            frame,
            ..
        } => handle_live(peer, session, subscribed, workspace_id, frame).await,
        env @ (Envelope::CompactVolunteer { .. } | Envelope::Compacted { .. }) => {
            handle_compaction(peer, session, subscribed, env).await;
            Ok(None)
        }
        env @ (Envelope::VoiceCall { .. }
        | Envelope::VoiceJoin { .. }
        | Envelope::VoiceLeave { .. }
        | Envelope::VoiceCalls { .. }) => handle_voice(peer, session, subscribed, env).await,
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::CompactRequest { .. }
        | Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. }
        | Envelope::RemoteActionUndeliverable { .. }
        | Envelope::Committed { .. }
        | Envelope::CommitRefused { .. }
        | Envelope::LiveMember { .. }
        | Envelope::Position { .. }
        | Envelope::VoiceCallResult { .. }
        | Envelope::VoiceGranted { .. }
        | Envelope::VoiceRefused { .. }
        | Envelope::VoiceCallList { .. } => Ok(None),
    }
}

pub(super) async fn handle_private(
    frame: EnvelopeV2,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    match frame {
        EnvelopeV2::State(Envelope::Ping) => Ok(Some(EnvelopeV2::State(Envelope::Pong))),
        EnvelopeV2::EnableActions { room } => {
            if room.is_nil() || !current_action_peer(peer, session) {
                return Err(PeerError::Protocol(
                    "private capability not authorised".into(),
                ));
            }
            let Some(ws) = private_workspace(room, peer, session).await? else {
                return Err(PeerError::Protocol("private room not subscribed".into()));
            };
            let mut wr =
                private_membership(&ws, peer, session, "private room not subscribed").await?;
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: session.outbound.clone(),
                    tx: session.private_tx.clone(),
                },
            );
            drop(wr);
            Ok(Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::TransportOnly,
            }))
        }
        EnvelopeV2::EnableBrokerClient { room } => {
            if !current_action_peer(peer, session) {
                return Err(PeerError::Protocol("broker protocol not authorised".into()));
            }
            let ws = private_workspace(room, peer, session)
                .await?
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr =
                private_membership(&ws, peer, session, "broker protocol not authorised").await?;
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_protocol = true;
            drop(wr);
            Ok(None)
        }
        EnvelopeV2::RegisterBroker { room, host } => {
            if host.is_nil() || !current_action_peer(peer, session) {
                return Err(PeerError::Protocol("broker not authorised".into()));
            }
            let ws = private_workspace(room, peer, session)
                .await?
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr = private_membership(&ws, peer, session, "broker not authorised").await?;
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_host = Some(host);
            target.broker_protocol = true;
            drop(wr);
            Ok(Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::DurableBroker { host },
            }))
        }
        EnvelopeV2::Directed(action) => {
            let reason = route_private(&action, peer, session).await?;
            Ok(reason.map(|reason| EnvelopeV2::unavailable(&action, reason)))
        }
        // The relay forwards the source's signed bytes; it never signs. A
        // signature that is not this session's device's is refused here, so
        // it cannot reach (and close) the target's attachment.
        EnvelopeV2::SignedDirected(signed) => {
            let action = signed.action();
            if signed.verify(&peer.device_public_key).is_err() {
                private_phase(action, "route_rejected_signature");
                return Ok(Some(EnvelopeV2::unavailable(
                    action,
                    UnavailableReason::NotAuthorised,
                )));
            }
            let reason = route_private_signed(action, Some(&signed), peer, session).await?;
            Ok(reason.map(|reason| EnvelopeV2::unavailable(action, reason)))
        }
        // Peers may send private results only inside a source-bound Directed
        // frame. Relay control responses can never be forged by a peer.
        _ => Err(PeerError::Protocol(
            "unexpected private control frame".into(),
        )),
    }
}
