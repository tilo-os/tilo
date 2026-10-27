//! Invoking what a relay answers itself as this device, on a relay session,
//! addressed to the relay's own device: a room's own governance
//! (`tilo.workspace`), a member's own business in it (`tilo.member`) and
//! the host's own operations (`tilo.host`). It is the action surface
//! agents, scripts and screen readers use, which the relay decides by the
//! same `Rooms::authorize` its other doors do.
//!
//! A room's governance goes on the room's relay session to the relay device
//! the room names. The host's operations go to a host by its endpoint, on
//! that host's session of a workspace this device holds there (its person's
//! continuity workspace), to the device that session's handshake
//! authenticated; such a request waits here until the session is up.
//!
//! The answer is taken here, in the actor, rather than on the compositor
//! thread that delivers every other remote-action answer: the shell asks
//! from that thread and waits for the answer, so it could never arrive.
//! Only a receipt for an invocation this sent, from the exact relay device
//! on the exact workspace it was sent to, is taken; everything else goes on
//! to the shell as before.
use std::collections::HashMap;
use std::time::{Duration, Instant};

use iroh::EndpointId;
use tilo_actions::InvocationId;
use tilo_actions::remote::{InvokeRequest, ReceiptStatus, RemoteActionFrame, RemoteActionMessage};

/// Most invocations waiting for their answer at once.
const MAX_WAITING: usize = 32;
/// How long an unanswered invocation is remembered; its caller stopped
/// waiting long before.
const FORGET_AFTER: Duration = Duration::from_secs(60);

/// Who an invocation is addressed to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addressee {
    /// The relay's own device, which a room names as its governor: on
    /// whichever relay session of the workspace reaches it.
    Device(EndpointId),
    /// The host at this endpoint, on its own session of the workspace: its
    /// device is the one that session's handshake authenticated.
    Host(EndpointId),
}

/// One invocation and where its answer goes.
#[derive(Debug)]
pub struct Request {
    /// The workspace topic whose relay session carries it.
    pub ws: u64,
    /// Who answers it.
    pub to: Addressee,
    /// The invocation; its target names the provider.
    pub invoke: InvokeRequest,
    /// Where the receipt goes.
    pub reply: std::sync::mpsc::SyncSender<Result<ReceiptStatus, String>>,
}

struct Waiter {
    ws: u64,
    host: EndpointId,
    since: Instant,
    reply: std::sync::mpsc::SyncSender<Result<ReceiptStatus, String>>,
}

/// The invocations waiting for their relay's answer, and those waiting for
/// their host's session to send them on.
#[derive(Default)]
pub(super) struct Waiting {
    entries: HashMap<InvocationId, Waiter>,
    unsent: Vec<(Instant, Request)>,
}

impl Waiting {
    /// Hold `request`, addressed to a host whose session of its workspace
    /// is not up yet, until [`Self::ready_for`] it.
    pub(super) fn hold(&mut self, request: Request, now: Instant) {
        self.forget(now);
        if self.unsent.len() >= MAX_WAITING {
            let _ = request
                .reply
                .send(Err("too many requests waiting for a relay".into()));
            return;
        }
        self.unsent.push((now, request));
    }

    /// The held requests for the host at `host` on `ws`, whose session just
    /// came up.
    pub(super) fn ready_for(&mut self, ws: u64, host: EndpointId) -> Vec<Request> {
        let (ready, held) = std::mem::take(&mut self.unsent)
            .into_iter()
            .partition(|(_, request)| request.ws == ws && request.to == Addressee::Host(host));
        self.unsent = held;
        ready.into_iter().map(|(_, request)| request).collect()
    }

    /// Drop what has waited too long: its caller stopped waiting.
    fn forget(&mut self, now: Instant) {
        self.entries
            .retain(|_, waiter| now.saturating_duration_since(waiter.since) < FORGET_AFTER);
        self.unsent
            .retain(|(since, _)| now.saturating_duration_since(*since) < FORGET_AFTER);
    }

    /// Remember `request`, sent to the relay device `host`, and answer the
    /// frame to send for it, or `None` when it was already answered with a
    /// refusal.
    pub(super) fn send(
        &mut self,
        request: Request,
        host: EndpointId,
        now: Instant,
    ) -> Option<Vec<u8>> {
        let Request {
            ws, invoke, reply, ..
        } = request;
        self.forget(now);
        let invocation = invoke.invocation;
        if self.entries.len() >= MAX_WAITING || self.entries.contains_key(&invocation) {
            let _ = reply.send(Err("too many requests to the room's relay".into()));
            return None;
        }
        let Ok(frame) = RemoteActionFrame::new(RemoteActionMessage::Invoke(invoke)).encode() else {
            let _ = reply.send(Err("the governance request could not be encoded".into()));
            return None;
        };
        self.entries.insert(
            invocation,
            Waiter {
                ws,
                host,
                since: now,
                reply,
            },
        );
        Some(frame)
    }

    /// Whether `event` from the relay session of `ws` answers an invocation
    /// waiting here; if so it went to that invocation's caller.
    pub(super) fn takes(&mut self, ws: u64, event: &super::HostEvent) -> bool {
        match event {
            super::HostEvent::Message {
                author,
                msg: super::NeighbourMsg::RemoteAction { frame },
                ..
            } => self.answer(ws, *author, frame),
            _ => false,
        }
    }

    /// Whether `frame` from `author` on `ws` answers an invocation waiting
    /// here; if so it went to that invocation's caller.
    pub(super) fn answer(&mut self, ws: u64, author: EndpointId, frame: &[u8]) -> bool {
        if self.entries.is_empty() {
            return false;
        }
        let Ok(frame) = RemoteActionFrame::decode(frame) else {
            return false;
        };
        let RemoteActionMessage::Receipt(receipt) = frame.message else {
            return false;
        };
        let ours = self
            .entries
            .get(&receipt.invocation)
            .is_some_and(|waiter| waiter.ws == ws && waiter.host == author);
        if !ours {
            return false;
        }
        if let Some(waiter) = self.entries.remove(&receipt.invocation) {
            let _ = waiter.reply.send(Ok(receipt.status));
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{InvocationReceipt, InvokeArgument, RemoteActionTarget};

    fn request(
        ws: u64,
        host: EndpointId,
    ) -> (
        Request,
        InvocationId,
        std::sync::mpsc::Receiver<Result<ReceiptStatus, String>>,
    ) {
        let (reply, answer) = std::sync::mpsc::sync_channel(1);
        let invocation = InvocationId::new();
        let invoke = InvokeRequest {
            invocation,
            target: RemoteActionTarget::room_governance("room", "invitation.list"),
            argument: InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        (
            Request {
                ws,
                to: Addressee::Device(host),
                invoke,
                reply,
            },
            invocation,
            answer,
        )
    }

    fn receipt(invocation: InvocationId) -> Vec<u8> {
        RemoteActionFrame::new(RemoteActionMessage::Receipt(InvocationReceipt {
            invocation,
            status: ReceiptStatus::Rejected {
                error: tilo_actions::remote::ActionHostError::UnknownAction,
            },
        }))
        .encode()
        .expect("receipt frame")
    }

    #[test]
    fn only_the_relay_asked_on_the_room_asked_answers_an_invocation() {
        let relay = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let mut waiting = Waiting::default();
        let (request, invocation, answer) = request(7, relay);
        let frame = waiting.send(request, relay, Instant::now()).expect("sent");
        let sent = RemoteActionFrame::decode(&frame).expect("invoke frame");
        let RemoteActionMessage::Invoke(sent) = sent.message else {
            panic!("an invocation is sent");
        };
        assert_eq!(sent.target.provider, "tilo.workspace");

        assert!(
            !waiting.answer(7, other, &receipt(invocation)),
            "another device"
        );
        assert!(
            !waiting.answer(8, relay, &receipt(invocation)),
            "another room"
        );
        assert!(
            !waiting.answer(7, relay, &receipt(InvocationId::new())),
            "another invocation"
        );
        assert!(answer.try_recv().is_err());

        assert!(waiting.answer(7, relay, &receipt(invocation)));
        assert_eq!(
            answer.try_recv().expect("answered"),
            Ok(ReceiptStatus::Rejected {
                error: tilo_actions::remote::ActionHostError::UnknownAction
            })
        );
        assert!(
            !waiting.answer(7, relay, &receipt(invocation)),
            "answered once"
        );
    }

    #[test]
    fn an_abandoned_invocation_is_forgotten() {
        let relay = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let mut waiting = Waiting::default();
        let start = Instant::now();
        let (first, invocation, _answer) = request(7, relay);
        waiting.send(first, relay, start).expect("sent");
        let (second, _, _second_answer) = request(7, relay);
        let later = start
            .checked_add(FORGET_AFTER.saturating_add(Duration::from_secs(1)))
            .expect("a later instant");
        waiting.send(second, relay, later).expect("sent");
        assert!(!waiting.answer(7, relay, &receipt(invocation)));
    }

    #[test]
    fn a_request_for_a_host_waits_for_that_hosts_session_of_its_workspace() {
        let host = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let other = iroh::SecretKey::from_bytes(&[2; 32]).public();
        let mut waiting = Waiting::default();
        let start = Instant::now();
        let (mut held, invocation, answer) = request(7, host);
        held.to = Addressee::Host(host);
        waiting.hold(held, start);
        assert!(waiting.ready_for(8, host).is_empty(), "another workspace");
        assert!(waiting.ready_for(7, other).is_empty(), "another host");
        let ready = waiting.ready_for(7, host);
        assert_eq!(ready.len(), 1);
        assert_eq!(
            ready.first().map(|request| request.invoke.invocation),
            Some(invocation)
        );
        assert!(waiting.ready_for(7, host).is_empty(), "released once");

        let (mut stale, _, _stale_answer) = request(7, host);
        stale.to = Addressee::Host(host);
        waiting.hold(stale, start);
        let later = start
            .checked_add(FORGET_AFTER.saturating_add(Duration::from_secs(1)))
            .expect("a later instant");
        let (fresh, _, _fresh_answer) = request(9, host);
        waiting.hold(fresh, later);
        assert!(waiting.ready_for(7, host).is_empty());
        drop(answer);
    }
}
