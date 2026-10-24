//! The session's writer: the only task that writes to the wire. It drains
//! private replies, replay batches, live frames, state runs and presence in
//! a fixed order of fairness, and completes every write it starts.

use crate::peer::{
    AsyncWriteExt, Envelope, PeerError, PrivateOutbound, ReplayWrite, STATE_RUN, SessionCoding,
    StateDelivery, authorize_state_frame, delivery_room, encode_charged, finish_replay_write, mpsc,
    trace_native_publication, write_presence, write_private, write_state_deliveries,
};

use super::SessionLoop;

/// How many private frames the writer writes before one available
/// state or replay frame.
const PRIVATE_BURST: usize = 8;

/// A write this long is logged: every frame queued behind it, a heartbeat's
/// answer among them, waited as long, and a peer that hears nothing for its
/// silence limit reconnects.
const SLOW_WRITE: std::time::Duration = std::time::Duration::from_secs(2);

/// What one pass over the ready queues did.
enum WriterTurn {
    /// A frame was written; look at the queues again.
    Wrote,
    /// No queue was ready; `live` says whether live frames may be written.
    Idle { live: bool },
}

/// The queues the session's reader and rooms feed its writer through.
pub(super) struct WriterQueues<'s> {
    /// Envelopes every room this peer is subscribed to queues for the wire.
    rx: &'s mut mpsc::Receiver<Envelope>,
    state_rx: &'s mut mpsc::Receiver<StateDelivery>,
    reply_rx: &'s mut mpsc::Receiver<ReplayWrite>,
    private_rx: &'s mut mpsc::Receiver<PrivateOutbound>,
}

impl<'s> WriterQueues<'s> {
    pub(super) const fn new(
        rx: &'s mut mpsc::Receiver<Envelope>,
        state_rx: &'s mut mpsc::Receiver<StateDelivery>,
        reply_rx: &'s mut mpsc::Receiver<ReplayWrite>,
        private_rx: &'s mut mpsc::Receiver<PrivateOutbound>,
    ) -> Self {
        Self {
            rx,
            state_rx,
            reply_rx,
            private_rx,
        }
    }
}

/// The writer state machine. It borrows the session's queues, its egress
/// and its presence mailbox; what it owns (the fairness state and the
/// connection's slot coding) ends with it.
pub(super) struct SessionWriter<'s, W> {
    rx: &'s mut mpsc::Receiver<Envelope>,
    state_rx: &'s mut mpsc::Receiver<StateDelivery>,
    reply_rx: &'s mut mpsc::Receiver<ReplayWrite>,
    private_rx: &'s mut mpsc::Receiver<PrivateOutbound>,
    write: &'s mut W,
    presence: &'s crate::presence_mailbox::PresenceMailbox,
    active: Option<ReplayWrite>,
    private_burst: usize,
    deferred_state: Option<StateDelivery>,
    deferred_frame: Option<Envelope>,
    raw_turn: bool,
    /// What the last turn wrote, for the slow-write log.
    wrote: &'static str,
    /// This connection's halves of its presence and delta slot coding:
    /// they see every batch and coded delta this writer writes, in order.
    coding: SessionCoding,
}

impl<'s, W: tokio::io::AsyncWrite + Unpin> SessionWriter<'s, W> {
    pub(super) fn new(
        WriterQueues {
            rx,
            state_rx,
            reply_rx,
            private_rx,
        }: WriterQueues<'s>,
        write: &'s mut W,
        presence: &'s crate::presence_mailbox::PresenceMailbox,
    ) -> Self {
        Self {
            rx,
            state_rx,
            reply_rx,
            private_rx,
            write,
            presence,
            active: None,
            private_burst: 0,
            deferred_state: None,
            deferred_frame: None,
            raw_turn: false,
            wrote: "",
            coding: SessionCoding::default(),
        }
    }

    /// Write until a write fails; the session ends the writer otherwise.
    pub(super) async fn run(mut self, cx: SessionLoop<'_, '_>) -> Result<(), PeerError> {
        loop {
            let started = std::time::Instant::now();
            let turn = self.write_ready(cx).await?;
            let held = started.elapsed();
            if held >= SLOW_WRITE {
                tracing::info!(
                    write = self.wrote,
                    held_ms = held.as_millis(),
                    "a write held the session writer"
                );
            }
            if let WriterTurn::Idle { live } = turn {
                // None of the queues was ready. Await readiness without starting
                // a write in a cancellable select branch.
                self.await_ready(live, cx).await?;
            }
        }
    }

    /// Write the next frame whose queue is ready, in fairness order.
    async fn write_ready(&mut self, cx: SessionLoop<'_, '_>) -> Result<WriterTurn, PeerError> {
        if self.active.is_none() {
            self.active = self.reply_rx.try_recv().ok();
        }
        // At most eight private frames before one available state/replay
        // frame. A single writer completes every frame before switching.
        if self.private_burst < PRIVATE_BURST
            && let Ok(private) = self.private_rx.try_recv()
        {
            self.wrote = "private";
            write_private(private, cx.peer, cx.session, self.write).await?;
            self.private_burst = self.private_burst.saturating_add(1);
            return Ok(WriterTurn::Wrote);
        }
        if let Some(batch) = self.active.take() {
            self.wrote = if batch.ticket.is_some() {
                "replay"
            } else {
                "reply"
            };
            Box::pin(finish_replay_write(
                batch,
                cx.peer,
                cx.session,
                cx.pending_replays,
                self.private_rx,
                self.write,
            ))
            .await?;
            cx.replay_written.notify_waiters();
            self.private_burst = 0;
            return Ok(WriterTurn::Wrote);
        }
        let live = cx.pending_replays.load(std::sync::atomic::Ordering::SeqCst) == 0;
        if live && self.write_live(cx).await? {
            return Ok(WriterTurn::Wrote);
        }
        Ok(WriterTurn::Idle { live })
    }

    /// With no replay pending: a live frame or a state run, taking turns,
    /// then due presence. `true` when one was written.
    async fn write_live(&mut self, cx: SessionLoop<'_, '_>) -> Result<bool, PeerError> {
        if self.deferred_frame.is_none() {
            self.deferred_frame = self.rx.try_recv().ok();
        }
        if self.deferred_state.is_none() {
            self.deferred_state = self.state_rx.try_recv().ok();
        }
        if (self.raw_turn || self.deferred_state.is_none())
            && let Some(frame) = self.deferred_frame.take()
        {
            self.wrote = "live";
            authorize_state_frame(&frame, cx.peer, cx.session).await?;
            // Presence and host-authored deltas are fan-out; the
            // rest of this lane answers or addresses one device.
            let live = matches!(frame, Envelope::Delta { .. } | Envelope::Presence { .. });
            self.write
                .write_all(&encode_charged(&frame, cx.session, live)?)
                .await?;
            trace_native_publication("written", &frame, Some(&cx.peer.device_public_key));
            self.private_burst = 0;
            self.raw_turn = false;
            return Ok(true);
        }
        if let Some(state) = self.deferred_state.take() {
            self.wrote = "state";
            write_state_deliveries(
                self.state_run(state),
                cx.peer,
                cx.session,
                &mut self.coding,
                self.presence,
                self.write,
            )
            .await?;
            self.private_burst = 0;
            self.raw_turn = true;
            return Ok(true);
        }
        // Presence goes once no delta is waiting: it never delays one.
        if self
            .presence
            .next_due()
            .is_some_and(|due| due <= tokio::time::Instant::now())
        {
            self.wrote = "presence";
            write_presence(
                self.presence,
                &mut self.coding.presence,
                cx.peer,
                cx.session,
                self.write,
            )
            .await?;
            self.private_burst = 0;
            return Ok(true);
        }
        Ok(false)
    }

    /// `state` and what else is queued for the same room goes with it, up to
    /// [`STATE_RUN`]; the first delivery for another room waits its turn.
    fn state_run(&mut self, state: StateDelivery) -> Vec<StateDelivery> {
        let room = delivery_room(&state);
        let mut run = vec![state];
        while run.len() < STATE_RUN {
            match self.state_rx.try_recv() {
                Ok(next) if room.is_some() && delivery_room(&next) == room => {
                    run.push(next);
                }
                Ok(next) => {
                    self.deferred_state = Some(next);
                    break;
                }
                Err(_) => break,
            }
        }
        run
    }

    /// Wait until a queue is ready. Only a private frame is written here,
    /// and it is written after its branch was chosen, so no write starts
    /// inside a branch the select could cancel.
    async fn await_ready(&mut self, live: bool, cx: SessionLoop<'_, '_>) -> Result<(), PeerError> {
        tokio::select! {
            biased;
            batch = self.reply_rx.recv() => { self.active = batch; }
            private = self.private_rx.recv() => {
                if let Some(private) = private { write_private(private, cx.peer, cx.session, self.write).await?; self.private_burst = 1; }
            }
            state = self.state_rx.recv(), if live => {
                self.deferred_state = state;
            }
            frame = self.rx.recv(), if live => {
                self.deferred_frame = frame;
            }
            () = self.presence.ready(), if live => {}
        }
        Ok(())
    }
}
