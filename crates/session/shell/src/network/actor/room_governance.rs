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
    /// Who waits for the last session of a workspace to close
    /// ([`Self::watch_sessions`]).
    watchers: Vec<(u64, tokio::sync::oneshot::Sender<()>)>,
    /// Invocations for a workspace whose session is being established,
    /// sent once it is up ([`Self::hold_for_session`]).
    connecting: Vec<Request>,
}

/// Where a workspace stands with its hosts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Sessions {
    /// A session is up.
    Open,
    /// None is up, and one is being established.
    Connecting,
    /// None is up, and none is being established.
    Closed,
}

/// What became of invocations to a room's host, kept in tests only, so a
/// test that times out waiting on one says where it stopped: output from
/// the network's threads is not captured with the test's.
pub(crate) mod trace {
    #[cfg(test)]
    static SAID: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

    /// Note `line`, in tests.
    #[cfg_attr(
        not(test),
        expect(unused_variables, reason = "only tests keep what is noted")
    )]
    pub(crate) fn note(line: impl FnOnce() -> String) {
        #[cfg(test)]
        {
            let started = *STARTED.get_or_init(std::time::Instant::now);
            let mut said = SAID
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            // The latest lines say where a wait stopped.
            if said.len() >= 400 {
                said.remove(0);
            }
            said.push(format!(
                "{:>8.3}s {}",
                started.elapsed().as_secs_f64(),
                line()
            ));
        }
    }

    #[cfg(test)]
    static STARTED: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();

    /// Keep every warning logged in this process from now on with what is
    /// noted, from the network's threads and the hosts' as well, and what
    /// the hosts say of their work.
    #[cfg(test)]
    pub(crate) fn capture_warnings() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        struct Lines;
        impl std::io::Write for Lines {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let line = String::from_utf8_lossy(bytes).trim_end().to_owned();
                note(|| line);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        INSTALLED.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_env_filter(tracing_subscriber::EnvFilter::new(
                    "warn,tilo_room_host=info",
                ))
                .with_writer(|| Lines)
                .with_ansi(false)
                .without_time()
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
    }

    /// Everything noted so far, a line each.
    #[cfg(test)]
    pub(crate) fn said() -> String {
        SAID.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .fold(String::new(), |mut said, line| {
                said.push_str("\n    ");
                said.push_str(line);
                said
            })
    }
}

/// Why an invocation sent on a workspace's sessions has no answer coming.
const SESSIONS_CLOSED: &str = "the connection to the room's host was lost";

impl Waiting {
    /// Whether anything here waits on where a workspace stands with its
    /// hosts ([`Self::sessions_changed`]).
    pub(super) fn waits_on_sessions(&self) -> bool {
        !self.entries.is_empty() || !self.watchers.is_empty() || !self.connecting.is_empty()
    }

    /// Tell `ended` once `ws` has no session and none is being
    /// established, as [`Self::sessions_changed`] finds: at once when that
    /// is so now.
    pub(super) fn watch_sessions(
        &mut self,
        ws: u64,
        now: Sessions,
        ended: tokio::sync::oneshot::Sender<()>,
    ) {
        if now == Sessions::Closed {
            let _ = ended.send(());
        } else {
            self.watchers.push((ws, ended));
        }
    }

    /// The workspaces something here waits on.
    pub(super) fn workspaces(&self) -> std::collections::BTreeSet<u64> {
        self.entries
            .values()
            .map(|waiter| waiter.ws)
            .chain(self.watchers.iter().map(|(ws, _)| *ws))
            .chain(self.connecting.iter().map(|request| request.ws))
            .collect()
    }

    /// Hold `request`, addressed to the relay device of a workspace whose
    /// session is being established, until [`Self::sessions_changed`] finds
    /// the session up (and answers it to send) or the attempt over without
    /// one (and fails it).
    pub(super) fn hold_for_session(&mut self, request: Request) {
        self.connecting.push(request);
    }

    /// Where workspaces stand with their hosts changed, as `sessions` says
    /// for each. An answer only ever comes on a session of its workspace,
    /// so an invocation sent on one that has none left is failed. Who
    /// watched a workspace with no session that none is being established
    /// for is told, and what was held for its session is failed. What was
    /// held for a workspace whose session is up is answered, to send.
    pub(super) fn sessions_changed(&mut self, sessions: impl Fn(u64) -> Sessions) -> Vec<Request> {
        self.entries.retain(|_, waiter| {
            let keep = sessions(waiter.ws) == Sessions::Open;
            if !keep {
                trace::note(|| format!("sent invocation failed: ws {} has no session", waiter.ws));
                let _ = waiter.reply.send(Err(SESSIONS_CLOSED.into()));
            }
            keep
        });
        let watchers = std::mem::take(&mut self.watchers);
        for (ws, ended) in watchers {
            if sessions(ws) == Sessions::Closed || ended.is_closed() {
                let _ = ended.send(());
            } else {
                self.watchers.push((ws, ended));
            }
        }
        let mut ready = Vec::new();
        for request in std::mem::take(&mut self.connecting) {
            match sessions(request.ws) {
                Sessions::Open => ready.push(request),
                Sessions::Connecting => self.connecting.push(request),
                Sessions::Closed => {
                    trace::note(|| {
                        format!(
                            "held {} failed: ws {} has no session",
                            request.invoke.invocation, request.ws
                        )
                    });
                    let _ = request.reply.send(Err(SESSIONS_CLOSED.into()));
                }
            }
        }
        ready
    }

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
        self.entries.retain(|invocation, waiter| {
            let keep = now.saturating_duration_since(waiter.since) < FORGET_AFTER;
            if !keep {
                trace::note(|| format!("forgot {invocation}"));
            }
            keep
        });
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
        trace::note(|| {
            format!(
                "receipt for {} from {author} on ws {ws}: {}, {:?}",
                receipt.invocation,
                if ours { "ours" } else { "not ours" },
                receipt.status
            )
        });
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

    /// What waits on a room's host ends when the room's last session does:
    /// an invocation sent there is failed, and who watched is told, while
    /// another room's still waits. A room whose session is being
    /// established keeps its watchers, and what was held for its session
    /// is sent once it is up, or failed once the attempt ends without one.
    #[test]
    fn the_last_session_of_a_room_closing_ends_what_waited_on_it() {
        let relay = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let mut waiting = Waiting::default();
        assert!(!waiting.waits_on_sessions());
        let (closing, _, closing_answer) = request(7, relay);
        let (staying, _, staying_answer) = request(8, relay);
        waiting.send(closing, relay, Instant::now()).expect("sent");
        waiting.send(staying, relay, Instant::now()).expect("sent");
        let (watch, mut closed) = tokio::sync::oneshot::channel();
        waiting.watch_sessions(7, Sessions::Open, watch);
        let (watch, mut other_closed) = tokio::sync::oneshot::channel();
        waiting.watch_sessions(8, Sessions::Open, watch);
        assert!(waiting.waits_on_sessions());

        let sent = waiting.sessions_changed(|ws| {
            if ws == 7 {
                Sessions::Closed
            } else {
                Sessions::Open
            }
        });
        assert!(sent.is_empty());
        assert!(
            closing_answer
                .try_recv()
                .expect("the closed room's invocation is answered")
                .is_err()
        );
        assert!(closed.try_recv().is_ok(), "who watched the room is told");
        assert!(
            staying_answer.try_recv().is_err(),
            "the other room's still waits"
        );
        assert!(other_closed.try_recv().is_err());

        let (watch, mut none) = tokio::sync::oneshot::channel();
        waiting.watch_sessions(9, Sessions::Closed, watch);
        assert!(
            none.try_recv().is_ok(),
            "a room with no session ends at once"
        );
    }

    /// An invocation for a room whose session is being established waits
    /// for it: sent once the session is up, failed when the attempt ends
    /// without one, and a watcher of the room is told only then.
    #[test]
    fn an_invocation_waits_for_a_session_being_established() {
        let relay = iroh::SecretKey::from_bytes(&[1; 32]).public();
        let mut waiting = Waiting::default();
        let (connects, invocation, connects_answer) = request(7, relay);
        let (fails, _, fails_answer) = request(8, relay);
        waiting.hold_for_session(connects);
        waiting.hold_for_session(fails);
        let (watch, mut ended) = tokio::sync::oneshot::channel();
        waiting.watch_sessions(8, Sessions::Connecting, watch);

        assert!(
            waiting
                .sessions_changed(|_| Sessions::Connecting)
                .is_empty()
        );
        assert!(connects_answer.try_recv().is_err());
        assert!(fails_answer.try_recv().is_err());
        assert!(ended.try_recv().is_err(), "still connecting");

        let sent = waiting.sessions_changed(|ws| {
            if ws == 7 {
                Sessions::Open
            } else {
                Sessions::Closed
            }
        });
        assert_eq!(
            sent.iter()
                .map(|request| request.invoke.invocation)
                .collect::<Vec<_>>(),
            vec![invocation],
            "the room whose session came up sends what waited for it"
        );
        assert!(connects_answer.try_recv().is_err(), "sent, not answered");
        assert!(
            fails_answer
                .try_recv()
                .expect("the attempt that ended without a session is answered")
                .is_err()
        );
        assert!(ended.try_recv().is_ok());
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
