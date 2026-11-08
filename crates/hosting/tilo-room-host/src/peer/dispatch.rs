//! The inbound dispatcher: [`handle_envelope`] for the state lane
//! ([`Envelope`]), and [`unserved_private`] for the frames a v2 peer sends
//! that this build does not serve.

use super::{
    Arc, AuthenticatedPeer, Envelope, EnvelopeV2, PeerError, RemoteActionRoute, ReplayCatchup,
    ReplayHandle, ReplayTicket, SessionCtx, Subscription, handle_commit, handle_compaction,
    handle_content_want, handle_delta, handle_here, handle_host_action, handle_host_hello,
    handle_live_join, handle_open_record, handle_presence, handle_remote_action, handle_subscribe,
    handle_sync_request, handle_unsubscribe, handle_voice, handle_workspaces, route_record_content,
};
#[cfg(test)]
use super::{ReplayWrite, finish_replay_write};

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
    Box::pin(finish_replay_write(
        ReplayWrite {
            ticket,
            reply,
            replaying,
        },
        peer,
        session,
        &pending,
        write,
    ))
    .await
}

#[expect(
    clippy::too_many_lines,
    reason = "one arm per envelope kind: the dispatcher is a table, read top to bottom"
)]
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
        env @ Envelope::RetiredPresence { .. } => {
            handle_presence(peer, session, subscribed, env).await
        }
        env @ Envelope::RecordContentWant { .. } => {
            handle_content_want(peer, session, subscribed, env).await
        }
        Envelope::PeerRelayHello { hosted_workspaces } => {
            handle_host_hello(peer, session, hosted_workspaces).await
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
        env @ (Envelope::CompactVolunteer { .. } | Envelope::Compacted { .. }) => {
            handle_compaction(peer, session, subscribed, env).await;
            Ok(None)
        }
        env @ (Envelope::VoiceCall { .. }
        | Envelope::VoiceJoin { .. }
        | Envelope::VoiceLeave { .. }
        | Envelope::VoiceShow { .. }
        | Envelope::VoiceCalls { .. }) => handle_voice(peer, session, subscribed, env).await,
        env @ Envelope::OpenRecord { .. } => {
            handle_open_record(peer, session, subscribed, env).await
        }
        Envelope::RetiredLiveJoin { workspace_id } => {
            handle_live_join(peer, session, subscribed, workspace_id).await
        }
        Envelope::HostAction { frame } => Ok(handle_host_action(peer, session, &frame)),
        Envelope::WorkspacesRequest { request_id, frame } => {
            handle_workspaces(peer, session, request_id, &frame).await
        }
        Envelope::HereRequest { request_id, frame } => {
            handle_here(peer, session, request_id, &frame).await
        }
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
        | Envelope::LiveLeave { .. }
        | Envelope::RetiredLive { .. }
        | Envelope::RetiredLiveMember { .. }
        | Envelope::Position { .. }
        | Envelope::VoiceCallResult { .. }
        | Envelope::VoiceGranted { .. }
        | Envelope::VoiceRefused { .. }
        | Envelope::VoiceCallList { .. }
        | Envelope::RecordOpened { .. }
        | Envelope::WorkspaceBootstrap { .. }
        | Envelope::WorkspacesReply { .. }
        | Envelope::HereReply { .. }
        | Envelope::FetchGrant { .. } => Ok(None),
    }
}

/// The v2 lane carries state frames and heartbeats only. Actions reach
/// another device on its own connection now, so a peer that sends one of
/// their frames speaks a protocol this build does not serve: the session
/// ends rather than dropping what it cannot route.
pub(super) fn unserved_private(frame: &EnvelopeV2) -> PeerError {
    tracing::warn!(
        retired = frame.is_retired(),
        "peer session closed: a peer sent an action frame this build does not serve"
    );
    PeerError::Protocol("action frames are not served on this connection".into())
}
