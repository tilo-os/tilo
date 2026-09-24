//! Private commands, status, cancel, catalogue and reads, plus the private
//! events poll, over the host's actual action runtime. Broker replies are
//! asynchronous: each original request is parked on its admission
//! continuation and answered only after a fresh owner revalidation.
use super::*;
use crate::{
    action_host::ActionHost,
    delivery_http::revalidation::PendingOperation,
    https::ResponseCredits,
    private_http::{NativePendingPrivate, NativePrivateService},
};
use qos_action_broker::peer_actions::PolicyTime;
use tokio::sync::OwnedSemaphorePermit;

/// Parked command replies and held event polls across all sessions.
const REPLIES: usize = 64;
const POLLS: usize = 16;

struct Reply {
    original: PendingOperation,
    pending: NativePendingPrivate,
    slot: OwnedSemaphorePermit,
}
struct HeldPoll {
    original: PendingOperation,
    slot: OwnedSemaphorePermit,
    /// Committed generation and time of the last full revalidation that found
    /// nothing ready (see `HELD_POLL_RECHECK`).
    checked: Option<(u64, tokio::time::Instant)>,
}
/// Held events polls are fully revalidated and re-polled only when an event
/// is ready, the committed generation changed, their deadline is near, or
/// this long after their last check. Before, every owner-loop turn (10 ms
/// tick and every request) revalidated every held poll's descriptor and
/// device authority. Every answer is still preceded by a fresh revalidation.
const HELD_POLL_RECHECK: std::time::Duration = std::time::Duration::from_secs(1);

pub(super) struct Private {
    service: Option<(NativePrivateService, PolicyTime)>,
    replies: Vec<Reply>,
    polls: Vec<HeldPoll>,
}

impl Private {
    pub(super) fn new() -> Self {
        Self {
            service: None,
            replies: Vec::new(),
            polls: Vec::new(),
        }
    }

    pub(super) fn busy(&self) -> bool {
        !self.replies.is_empty() || !self.polls.is_empty() || self.service.is_some()
    }

    fn close(&mut self) {
        if let Some((mut service, _)) = self.service.take() {
            service.close();
        }
        // Dropping parked originals rejects them; no reply escapes a retired runtime.
        self.replies.clear();
        self.polls.clear();
    }

    /// Follow the host's current action runtime. A retired or replaced runtime
    /// closes the carrier and every parked request bound to it.
    pub(super) fn sync(&mut self, actions: Option<&ActionHost>) {
        let Some(actions) = actions else {
            self.close();
            return;
        };
        let time = actions.registered_policy_time();
        let stale = self
            .service
            .as_ref()
            .is_none_or(|(service, origin)| service.is_closed() || !origin.same_origin(time));
        if !stale {
            return;
        }
        self.close();
        match NativePrivateService::install(actions) {
            Ok(service) => self.service = Some((service, time.clone())),
            Err(error) => tracing::debug!(%error, "private HTTPS carrier unavailable"),
        }
    }

    pub(super) fn retire_session(&mut self, session: [u8; 16]) {
        if let Some((service, _)) = &mut self.service {
            service.retire_session(session);
        }
        self.replies
            .retain(|reply| reply.original.session_id() != session);
        self.polls
            .retain(|poll| poll.original.session_id() != session);
    }

    /// Submit one private command/read. The original request is parked until
    /// the broker replies; returns the request back on refusal.
    pub(super) fn command(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> std::result::Result<(), BufferedRequest> {
        let submitted = (|| -> Result<_> {
            let (service, _) = self
                .service
                .as_mut()
                .context("private carrier not installed")?;
            ensure!(self.replies.len() < REPLIES, "private replies full");
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let slot = sessions.reserve_private_slot(proof.session_id())?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            let (admitted, mut checker, turn) = sessions.admit_revalidating(
                owner,
                current,
                budget,
                OwnerDataOperation::PrivateContext {
                    input_len: request.body().len(),
                },
                &proof,
                &exact,
                &request,
            )?;
            let guard = current.read().map_err(anyhow::Error::msg)?;
            checker.check(owner, guard.view(), admitted.session())?;
            let ticket = checker.capture_continuation(&admitted, &request)?;
            let pending = service.submit(admitted, guard.view(), request.deadline())?;
            drop(turn);
            Ok((ticket, pending, slot))
        })();
        let (ticket, pending, slot) = match submitted {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "private request refused");
                return Err(request);
            }
        };
        // A replaced original is dropped by park, which rejects it.
        if let Ok(original) = ticket.park(request) {
            self.replies.push(Reply {
                original,
                pending,
                slot,
            });
        }
        Ok(())
    }

    /// Answer a ready private event now, or hold the original poll.
    pub(super) fn poll(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
        request: BufferedRequest,
    ) -> std::result::Result<(), BufferedRequest> {
        let outcome = (|| -> Result<_> {
            let (service, _) = self
                .service
                .as_mut()
                .context("private carrier not installed")?;
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let slot = sessions.reserve_private_slot(proof.session_id())?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            let (admitted, mut checker, turn) = sessions.admit_revalidating(
                owner,
                current,
                budget,
                OwnerDataOperation::PrivateContext { input_len: 0 },
                &proof,
                &exact,
                &request,
            )?;
            let guard = current.read().map_err(anyhow::Error::msg)?;
            checker.check(owner, guard.view(), admitted.session())?;
            let ticket = checker.capture_continuation(&admitted, &request)?;
            let ready = service.poll_events(admitted, guard.view())?;
            drop(turn);
            Ok((ticket, ready, slot))
        })();
        let (ticket, ready, slot) = match outcome {
            Ok(value) => value,
            Err(error) => {
                tracing::debug!(%error, "private events poll refused");
                return Err(request);
            }
        };
        if let Some((pending, reply)) = ready {
            let Ok(guard) = current.read() else {
                return Err(request);
            };
            let credits = ResponseCredits {
                private_slot: Some(slot),
                ..Default::default()
            };
            if let Err(error) = pending.respond(guard.view(), &reply, request, credits) {
                tracing::debug!(%error, "private event withheld");
            }
            return Ok(());
        }
        let session = ticket.session_id();
        // One held poll per session: the newer poll supersedes the older one.
        if let Some(index) = self
            .polls
            .iter()
            .position(|poll| poll.original.session_id() == session)
        {
            let old = self.polls.remove(index);
            let _ = old
                .original
                .into_request()
                .respond(StatusCode::OK, Vec::new());
        }
        if self.polls.len() >= POLLS {
            let _ = request.respond(StatusCode::OK, Vec::new());
            return Ok(());
        }
        if let Ok(original) = ticket.park(request) {
            self.polls.push(HeldPoll {
                original,
                slot,
                checked: None,
            });
        }
        Ok(())
    }

    /// Drive the carrier, answer broker replies and ready or expiring polls.
    pub(super) fn pump(
        &mut self,
        sessions: &mut HttpSessions,
        current: &CommittedAuthority,
        owner: &SessionOwner,
        budget: &HostBytes,
        idle: &super::idle_margin::IdleMargin,
    ) {
        let Some((service, _)) = self.service.as_mut() else {
            return;
        };
        if let Err(error) = service.pump() {
            tracing::debug!(%error, "private carrier closed");
            self.close();
            return;
        }
        let mut index = 0;
        while index < self.replies.len() {
            let entry = &mut self.replies[index];
            if entry.original.original().is_cancelled() {
                self.replies.swap_remove(index);
                continue;
            }
            let reply = match entry.pending.try_reply() {
                Ok(None) => {
                    index += 1;
                    continue;
                }
                Ok(Some(reply)) => reply,
                Err(error) => {
                    tracing::debug!(%error, "private reply unavailable");
                    let entry = self.replies.swap_remove(index);
                    refuse(entry.original.into_request());
                    continue;
                }
            };
            let entry = self.replies.swap_remove(index);
            let checked = (|| -> Result<()> {
                let (session, mut checker, turn) =
                    sessions.resume_operation(owner, current, budget, &entry.original)?;
                let guard = current.read().map_err(anyhow::Error::msg)?;
                checker.check(owner, guard.view(), session)?;
                service.observe_reply(&entry.pending, &reply, guard.view())?;
                drop(turn);
                Ok(())
            })();
            let Reply {
                original,
                pending,
                slot,
            } = entry;
            match checked.and_then(|()| current.read().map_err(anyhow::Error::msg)) {
                Ok(guard) => {
                    let credits = ResponseCredits {
                        private_slot: Some(slot),
                        ..Default::default()
                    };
                    if let Err(error) =
                        pending.respond(guard.view(), &reply, original.into_request(), credits)
                    {
                        tracing::debug!(%error, "private reply withheld");
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "private reply refused");
                    refuse(original.into_request());
                }
            }
        }
        let now = tokio::time::Instant::now();
        let generation = owner.committed_generation();
        let event_ready = service.has_ready_event();
        let mut index = 0;
        while index < self.polls.len() {
            if self.polls[index].original.original().is_cancelled() {
                self.polls.swap_remove(index);
                continue;
            }
            let deadline = self.polls[index].original.original().deadline();
            let before_expiry = idle.may_hold(deadline, now);
            let unchanged = self.polls[index].checked.is_some_and(|(checked, at)| {
                checked == generation && now.saturating_duration_since(at) < HELD_POLL_RECHECK
            });
            if !crate::action_pump_cadence::host_perf_baseline()
                && unchanged
                && before_expiry
                && !event_ready
            {
                index += 1;
                continue;
            }
            let ready = (|| -> Result<_> {
                let held = &self.polls[index];
                let (session, mut checker, turn) =
                    sessions.resume_operation(owner, current, budget, &held.original)?;
                let guard = current.read().map_err(anyhow::Error::msg)?;
                checker.check(owner, guard.view(), session)?;
                let ready =
                    service.poll_events_resumed(session, held.original.original(), guard.view())?;
                drop(turn);
                Ok(ready)
            })();
            match ready {
                Ok(Some((pending, reply))) => {
                    let HeldPoll { original, slot, .. } = self.polls.swap_remove(index);
                    let credits = ResponseCredits {
                        private_slot: Some(slot),
                        ..Default::default()
                    };
                    match current.read() {
                        Ok(guard) => {
                            if let Err(error) = pending.respond(
                                guard.view(),
                                &reply,
                                original.into_request(),
                                credits,
                            ) {
                                tracing::debug!(%error, "private event withheld");
                            }
                        }
                        Err(_) => refuse(original.into_request()),
                    }
                }
                Ok(None) if before_expiry => {
                    self.polls[index].checked = Some((generation, now));
                    index += 1;
                }
                // Deadline reached with nothing ready: an empty page means idle.
                Ok(None) => {
                    let held = self.polls.swap_remove(index);
                    let _ = held
                        .original
                        .into_request()
                        .respond(StatusCode::OK, Vec::new());
                }
                Err(error) => {
                    tracing::debug!(%error, "held private poll refused");
                    let held = self.polls.swap_remove(index);
                    refuse(held.original.into_request());
                }
            }
        }
    }
}
