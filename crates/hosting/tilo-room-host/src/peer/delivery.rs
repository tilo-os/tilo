//! Writing state to the wire: per-session encoding, presence batches, and
//! authorizing, writing and retiring state deliveries.

use super::{
    AsyncWriteExt, AuthenticatedPeer, Envelope, LiveEnvelope, PeerError, RoomOperation, SessionCtx,
    SharedWorkspace, StateDelivery, authorize_state_recipient, resume_record,
    trace_native_publication, unix_now_ns,
};

/// `envelope` as the relay stream carries it (a zero-signature delta goes
/// without its signature), for a frame about to be written, charged to its
/// room's egress. `live` marks live fan-out (a delta or presence forwarded as
/// it arrived), as opposed to replay, replies and positions.
pub(super) fn encode_charged(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
    live: bool,
) -> Result<Vec<u8>, postcard::Error> {
    let frame = tilo_delivery_protocol::transport_v2::encode_state(envelope)?;
    if let Some(room) = envelope.workspace_id() {
        session
            .metrics
            .add_room_egress(room, u64::try_from(frame.len()).unwrap_or(u64::MAX), live);
    }
    Ok(frame)
}

/// A live delta about to be written, charged to its room's live egress:
/// coded against what the session last received from the same author in the
/// same room (`DeltaEncoder`). A delta the encoder does not code (signed, or
/// larger than it keeps) goes as [`encode_charged`] writes it and changes no
/// slot.
fn encode_delta_charged(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
    deltas: &mut tilo_delivery_protocol::transport_v2::DeltaEncoder,
) -> Result<Vec<u8>, postcard::Error> {
    if let Some(frame) = deltas.encode(envelope)? {
        if let Some(room) = envelope.workspace_id() {
            session.metrics.add_room_egress(
                room,
                u64::try_from(frame.len()).unwrap_or(u64::MAX),
                true,
            );
        }
        return Ok(frame);
    }
    encode_charged(envelope, session, true)
}

/// Presence items one `PresenceBatch` frame carries at most.
const PRESENCE_BATCH_ITEMS: usize = 64;

/// One connection's writer-side slot coding: each half sees every frame of
/// its kind the writer writes, in order, as the reader's half does.
#[derive(Default)]
pub(super) struct SessionCoding {
    /// Presence batches.
    pub(super) presence: tilo_delivery_protocol::transport_v2::PresenceEncoder,
    /// Live deltas.
    deltas: tilo_delivery_protocol::transport_v2::DeltaEncoder,
}

/// Write the presence due for this connection, with everything else that
/// may ride along (`PresenceMailbox::take`), in one write.
pub(super) async fn write_presence<W: tokio::io::AsyncWrite + Unpin>(
    mailbox: &crate::presence_mailbox::PresenceMailbox,
    slots: &mut tilo_delivery_protocol::transport_v2::PresenceEncoder,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    let out = encode_presence(
        mailbox.take(tokio::time::Instant::now(), true),
        slots,
        peer,
        session,
    )
    .await?;
    if !out.is_empty() {
        write.write_all(&out).await?;
    }
    Ok(())
}

/// Encode `due` presence for this connection, each room's authorised once,
/// as slot-coded `PresenceBatch` frames. Charged to each room's live egress.
async fn encode_presence(
    due: Vec<crate::presence_mailbox::DuePresence>,
    slots: &mut tilo_delivery_protocol::transport_v2::PresenceEncoder,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Vec<u8>, PeerError> {
    let mut out = Vec::new();
    for run in due.chunk_by(|a, b| a.workspace_id == b.workspace_id) {
        let Some(room) = run.first().map(|presence| presence.workspace_id) else {
            continue;
        };
        authorize_state_recipient(room, true, peer, session).await?;
        let before = out.len();
        for chunk in run.chunks(PRESENCE_BATCH_ITEMS) {
            let items: Vec<_> = chunk
                .iter()
                .map(|presence| slots.item(presence.key, &presence.payload))
                .collect();
            out.extend(tilo_delivery_protocol::transport_v2::encode_presence_batch(
                room, &items,
            )?);
        }
        let bytes = u64::try_from(out.len().saturating_sub(before)).unwrap_or(u64::MAX);
        session.metrics.add_room_egress(room, bytes, true);
    }
    Ok(out)
}

/// The most queued deliveries of one room a writer takes in one turn.
pub(super) const STATE_RUN: usize = 32;

/// The room a live delivery is for; `None` for what is not a delta.
pub(super) fn delivery_room(delivery: &StateDelivery) -> Option<u64> {
    match &*delivery.envelope {
        Envelope::Delta { workspace_id, .. } => Some(*workspace_id),
        _ => None,
    }
}

#[cfg(test)]
pub(super) async fn write_state_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: StateDelivery,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    coding: &mut SessionCoding,
    presence: &crate::presence_mailbox::PresenceMailbox,
    write: &mut W,
) -> Result<(), PeerError> {
    write_state_deliveries(vec![delivery], peer, session, coding, presence, write).await
}

/// Write `run`, queued deliveries of one room in order, authorised under
/// one membership read: authorised one by one, a room's every commit took
/// its lock once per member, and a crowd's writers queued on it. A run whose
/// every delivery is still authorised as its fan-out authorised it
/// ([`StateDelivery::still_fanned`]) is written without the room's lock.
pub(super) async fn write_state_deliveries<W: tokio::io::AsyncWrite + Unpin>(
    run: Vec<StateDelivery>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    coding: &mut SessionCoding,
    presence: &crate::presence_mailbox::PresenceMailbox,
    write: &mut W,
) -> Result<(), PeerError> {
    let Some(workspace_id) = run.first().and_then(delivery_room) else {
        return Ok(());
    };
    if crate::persistence::correlated_diagnostics() {
        for delivery in &run {
            if let Envelope::Delta { authored_at_ns, .. } = &*delivery.envelope {
                tracing::info!(phase="server_delta_writer", workspace_id, authored_at_ns, recipient=%hex::encode(&peer.device_public_key[..4]));
            }
        }
    }
    let now = unix_now_ns();
    let (accepted, workspace) = if run.iter().all(|delivery| {
        delivery_room(delivery) == Some(workspace_id)
            && delivery.still_fanned(peer, session, workspace_id, now)
    }) {
        // None of them has queue entries to retire.
        (run, None)
    } else {
        let (accepted, workspace) =
            authorize_state_deliveries(run, workspace_id, peer, session).await?;
        (accepted, Some(workspace))
    };
    if accepted.is_empty() {
        return Ok(());
    }
    if crate::persistence::correlated_diagnostics() {
        for delivery in &accepted {
            if let Envelope::Delta { authored_at_ns, .. } = &*delivery.envelope {
                tracing::info!(phase="server_delta_wire", workspace_id, authored_at_ns, recipient=%hex::encode(&peer.device_public_key[..4]));
            }
        }
    }
    // One write, so one packet where it fits: the deltas, their position,
    // and the presence that may ride along, above all the author's caret and
    // typing for this keystroke, held for it.
    let mut out = Vec::new();
    for delivery in &accepted {
        out.extend(encode_delta_charged(
            &delivery.envelope,
            session,
            &mut coding.deltas,
        )?);
    }
    if let Some(position) = resume_record(
        session,
        workspace_id,
        accepted.iter().map(|delivery| &*delivery.envelope),
    ) {
        out.extend(encode_charged(&position, session, false)?);
    }
    out.extend(
        encode_presence(
            presence.take(tokio::time::Instant::now(), true),
            &mut coding.presence,
            peer,
            session,
        )
        .await?,
    );
    write.write_all(&out).await?;
    for delivery in &accepted {
        trace_native_publication("written", &delivery.envelope, Some(&peer.device_public_key));
    }
    let cleanups: Vec<_> = accepted
        .into_iter()
        .filter(|delivery| delivery.cleanup.is_some())
        .collect();
    match workspace {
        Some(workspace) => retire_written(cleanups, &workspace, peer, session).await,
        None => Ok(()),
    }
}

/// The deliveries of `run` the recipient may still be written, decided under
/// one membership read of its room, with the room.
async fn authorize_state_deliveries(
    run: Vec<StateDelivery>,
    workspace_id: u64,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(Vec<StateDelivery>, SharedWorkspace), PeerError> {
    let workspace = session
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("state workspace disappeared".into()))?;
    let mut runtime = crate::workspace_io::fresh_membership(&workspace).await?;
    if !session.admits(&runtime, peer, RoomOperation::State) {
        drop(runtime);
        return Ok((Vec::new(), workspace));
    }
    let mut accepted = Vec::with_capacity(run.len());
    for delivery in run {
        // A connection can unsubscribe and subscribe again on the same
        // channel. Check the captured lease after the membership wait, before
        // either writing or fencing: stale work must not affect the
        // replacement subscription.
        if delivery_room(&delivery) != Some(workspace_id)
            || !delivery.current_subscription(&runtime, peer, session.outbound)
        {
            continue;
        }
        if delivery
            .source
            .as_ref()
            .is_some_and(|source| !source.authorized(&runtime))
        {
            if matches!(&delivery.envelope, LiveEnvelope::Selected(_)) {
                // CURRENT already committed this activation. A missing half
                // cannot be retried live after semantic suppression; reconnect
                // needs the durable basis. The exact recipient lease was
                // checked above.
                let _ = session.disconnect.try_send(());
                runtime.remove_subscriber_session(&peer.device_public_key, session.outbound);
            }
            continue;
        }
        accepted.push(delivery);
    }
    drop(runtime);
    Ok((accepted, workspace))
}

/// Retire the durable queue entries of `written` deliveries. Enqueue is
/// never receipt: exact cleanup is admitted only after the complete frame
/// write, and is ordered against commits and replay.
async fn retire_written(
    written: Vec<StateDelivery>,
    workspace: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    if written.is_empty() {
        return Ok(());
    }
    let owner = crate::persistence::handle(workspace).await;
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if !session.admits(&runtime, peer, RoomOperation::State) {
        return Ok(());
    }
    for delivery in written {
        if !delivery.current_subscription(&runtime, peer, session.outbound) {
            continue;
        }
        let Some(path) = delivery.cleanup.clone() else {
            continue;
        };
        owner.read(1, move |_| {
            if !delivery.live.load(std::sync::atomic::Ordering::SeqCst) {
                return Ok(());
            }
            match std::fs::remove_file(path) {
                Ok(()) => Ok(()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(e) => Err(e.into()),
            }
        })?;
    }
    Ok(())
}
