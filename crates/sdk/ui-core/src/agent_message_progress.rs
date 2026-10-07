//! Bounded demand-driven public milestone reader. No private worker recovery,
//! inferred typing or autonomous frame polling is involved.
use crate::{
    collaboration::{CollabEvent, CollabRequest},
    time::{Duration, Instant},
    widget::Collaborative,
};
pub use runtime_collab::{
    AgentMessagePublicPhase, AgentMessagePublicProgress, AgentMessagePublicReason,
};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_VISIBLE: usize = 32;
const READ_INTERVAL: Duration = Duration::from_secs(2);
const READ_TIMEOUT: Duration = Duration::from_secs(10);
const OBSERVATION_WINDOW: Duration = Duration::from_secs(60);
const MAX_AUTOMATIC_READS: u8 = 30;
static NEXT_REQUEST: AtomicU64 = AtomicU64::new(1);

struct Entry {
    row: String,
    progress: Option<AgentMessagePublicProgress>,
    unavailable: bool,
    pending: Option<(u64, Instant)>,
    last_request: Option<Instant>,
    first_request: Option<Instant>,
    reads: u8,
    suspended: bool,
}

/// One reader collection per visible Chat view. Removing a parent cancels
/// locally queued work and discards late responses, not a model operation.
#[derive(Default)]
pub struct AgentMessageProgressReader {
    room: Option<uuid::Uuid>,
    visible: BTreeMap<uuid::Uuid, Entry>,
    queued: Vec<CollabRequest>,
}

impl AgentMessageProgressReader {
    /// Select only actual visible direct/detached originals. This queues no
    /// reads; call `refresh` on view opening or its bounded async timer.
    /// # Errors
    /// Too many, duplicate or noncanonical original rows; no partial update.
    pub fn set_visible(
        &mut self,
        room: uuid::Uuid,
        originals: &[(uuid::Uuid, String)],
    ) -> Result<(), String> {
        if room.is_nil() || originals.len() > MAX_VISIBLE {
            return Err("invalid public Message visibility selection".into());
        }
        let mut ids = BTreeSet::new();
        for (id, row) in originals {
            runtime_collab::validate_agent_message_progress_query(&id.to_string(), row)?;
            if !ids.insert(*id) {
                return Err("duplicate public Message visibility selection".into());
            }
        }
        if self.room != Some(room) {
            self.cancel();
            self.room = Some(room);
        }
        self.visible.retain(|id, _| ids.contains(id));
        self.queued.retain(|request| matches!(request, CollabRequest::ReadAgentMessageProgress { invocation, .. } if uuid::Uuid::parse_str(invocation).is_ok_and(|id| ids.contains(&id))));
        for (id, row) in originals {
            self.visible.entry(*id).or_insert_with(|| Entry {
                row: row.clone(),
                progress: None,
                unavailable: false,
                pending: None,
                last_request: None,
                first_request: None,
                reads: 0,
                suspended: false,
            });
        }
        Ok(())
    }

    /// Release all view-local demand; late replies are inert.
    pub fn cancel(&mut self) {
        self.room = None;
        self.visible.clear();
        self.queued.clear();
    }

    pub fn progress(&self, original: uuid::Uuid) -> Option<&AgentMessagePublicProgress> {
        self.visible.get(&original)?.progress.as_ref()
    }

    /// Whether an exact selected status read is already awaiting its reply.
    #[must_use]
    pub fn refresh_in_flight(&self) -> bool {
        self.visible.values().any(|entry| entry.pending.is_some())
    }
    pub fn unavailable(&self, original: uuid::Uuid) -> bool {
        self.visible
            .get(&original)
            .is_some_and(|entry| entry.unavailable)
    }

    /// The next bounded observation wake, not an animation request. A view
    /// calls `refresh` at this deadline; suspended or terminal originals need
    /// no timer and resume only through explicit user demand.
    pub fn next_wake_deadline(&self) -> Option<Instant> {
        self.room?;
        self.visible
            .values()
            .filter(|entry| {
                !entry.suspended
                    && !entry.progress.as_ref().is_some_and(|progress| {
                        matches!(
                            progress.phase,
                            AgentMessagePublicPhase::Committed { .. }
                                | AgentMessagePublicPhase::NeedsAttention(_)
                        )
                    })
            })
            .filter_map(|entry| {
                if let Some((_, sent)) = entry.pending {
                    return sent.checked_add(READ_TIMEOUT);
                }
                let next = entry
                    .last_request
                    .and_then(|sent| sent.checked_add(READ_INTERVAL))
                    .unwrap_or_else(crate::time::now);
                Some(
                    entry
                        .first_request
                        .and_then(|first| first.checked_add(OBSERVATION_WINDOW))
                        .map_or(next, |end| next.min(end)),
                )
            })
            .min()
    }

    /// Demand reads at most once per two seconds per selected original. An
    /// unavailable result does not erase the last truthful milestone. Terminal
    /// milestones, absence and unknown reads stop automatic demand. Quiet
    /// unchanged milestones have a finite 60s/30-read observation window.
    pub fn refresh(&mut self) {
        self.refresh_at(crate::time::now());
    }

    /// An explicit user refresh resets only read observation, never an effect,
    /// model turn, invocation identity, or retained truthful milestone.
    #[must_use]
    pub fn request_refresh(&mut self) -> bool {
        if self.refresh_in_flight() {
            return false;
        }
        let queued_before = self.queued.len();
        for entry in self.visible.values_mut() {
            entry.first_request = None;
            entry.last_request = None;
            entry.reads = 0;
            entry.suspended = false;
        }
        self.refresh();
        self.queued.len() > queued_before
    }

    fn refresh_at(&mut self, now: Instant) {
        let Some(room) = self.room else { return };
        for (id, entry) in &mut self.visible {
            if entry
                .pending
                .is_some_and(|(_, sent)| now.duration_since(sent) >= READ_TIMEOUT)
            {
                entry.pending = None;
                entry.unavailable = true;
                entry.suspended = true;
            }
            if entry.suspended
                || entry.pending.is_some()
                || entry.progress.as_ref().is_some_and(|progress| {
                    matches!(
                        progress.phase,
                        AgentMessagePublicPhase::Committed { .. }
                            | AgentMessagePublicPhase::NeedsAttention(_)
                    )
                })
                || entry
                    .last_request
                    .is_some_and(|sent| now.duration_since(sent) < READ_INTERVAL)
            {
                continue;
            }
            if entry.reads >= MAX_AUTOMATIC_READS
                || entry
                    .first_request
                    .is_some_and(|first| now.duration_since(first) >= OBSERVATION_WINDOW)
            {
                entry.unavailable = true;
                entry.suspended = true;
                continue;
            }
            let request_id = NEXT_REQUEST.fetch_add(1, Ordering::Relaxed);
            entry.pending = Some((request_id, now));
            entry.last_request = Some(now);
            entry.first_request.get_or_insert(now);
            entry.reads += 1;
            self.queued.push(CollabRequest::ReadAgentMessageProgress {
                request_id,
                room,
                invocation: id.to_string(),
                request_row: entry.row.clone(),
            });
        }
        // An undrained widget must not accumulate timed-out queued requests.
        self.queued.retain(|request| matches!(request, CollabRequest::ReadAgentMessageProgress { request_id, invocation, .. } if uuid::Uuid::parse_str(invocation).ok().and_then(|id| self.visible.get(&id)).is_some_and(|entry| entry.pending.is_some_and(|(id, _)| id == *request_id))));
    }
}

impl Collaborative for AgentMessageProgressReader {
    fn take_collab_requests(&mut self) -> Vec<CollabRequest> {
        std::mem::take(&mut self.queued)
    }
    fn handle_collab_event(&mut self, event: &CollabEvent) {
        let CollabEvent::AgentMessageProgress { request_id, result } = event else {
            return;
        };
        let Some(entry) = self
            .visible
            .values_mut()
            .find(|entry| entry.pending.is_some_and(|(id, _)| id == *request_id))
        else {
            return;
        };
        entry.pending = None;
        match result {
            Ok(Some(progress))
                if runtime_collab::validate_agent_message_public_progress(progress).is_ok()
                    && entry
                        .progress
                        .as_ref()
                        .is_none_or(|old| progress.generation >= old.generation) =>
            {
                entry.progress = Some(progress.clone());
                entry.unavailable = false;
            }
            Ok(None) if entry.progress.is_none() => {
                entry.unavailable = false;
                entry.suspended = true;
            }
            _ => {
                entry.unavailable = true;
                entry.suspended = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn original(number: u128) -> (uuid::Uuid, String) {
        let id = uuid::Uuid::from_u128(number);
        (id, format!("m_{}", id.simple()))
    }
    fn request(reader: &mut AgentMessageProgressReader) -> u64 {
        let requests = reader.take_collab_requests();
        let [CollabRequest::ReadAgentMessageProgress { request_id, .. }] = requests.as_slice()
        else {
            panic!("one real public read expected")
        };
        *request_id
    }
    fn respond(
        reader: &mut AgentMessageProgressReader,
        id: u64,
        phase: AgentMessagePublicPhase,
        generation: u64,
    ) {
        reader.handle_collab_event(&CollabEvent::AgentMessageProgress {
            request_id: id,
            result: Ok(Some(AgentMessagePublicProgress { phase, generation })),
        });
    }

    #[test]
    fn observation_deadlines_stop_when_demand_is_suspended_or_terminal() {
        let mut reader = AgentMessageProgressReader::default();
        assert!(reader.next_wake_deadline().is_none());
        let parent = original(61);
        reader
            .set_visible(uuid::Uuid::from_u128(60), std::slice::from_ref(&parent))
            .unwrap();
        let now = crate::time::now();
        reader.refresh_at(now);
        let id = request(&mut reader);
        assert_eq!(reader.next_wake_deadline(), Some(now + READ_TIMEOUT));
        respond(&mut reader, id, AgentMessagePublicPhase::WaitingForReply, 1);
        assert_eq!(reader.next_wake_deadline(), Some(now + READ_INTERVAL));
        reader.refresh_at(now + READ_INTERVAL);
        let id = request(&mut reader);
        respond(
            &mut reader,
            id,
            AgentMessagePublicPhase::Committed {
                reply_row: original(62).1,
            },
            2,
        );
        assert!(reader.next_wake_deadline().is_none());

        reader.cancel();
        reader
            .set_visible(uuid::Uuid::from_u128(60), std::slice::from_ref(&parent))
            .unwrap();
        reader.refresh_at(now);
        request(&mut reader);
        reader.refresh_at(now + READ_TIMEOUT);
        assert!(reader.unavailable(parent.0));
        assert!(reader.next_wake_deadline().is_none());
        reader.cancel();
        assert!(reader.next_wake_deadline().is_none());
    }

    #[test]
    fn demanded_reader_is_bounded_and_cancelled_responses_are_inert() {
        let mut reader = AgentMessageProgressReader::default();
        let room = uuid::Uuid::from_u128(10);
        let parent = original(11);
        reader
            .set_visible(room, std::slice::from_ref(&parent))
            .unwrap();
        assert!(
            reader.take_collab_requests().is_empty(),
            "selection is not frame polling"
        );
        let now = crate::time::now();
        reader.refresh_at(now);
        let old = request(&mut reader);
        reader.refresh_at(now + Duration::from_secs(1));
        assert!(
            reader.take_collab_requests().is_empty(),
            "one read per original in flight"
        );
        let overflow = (20..53).map(original).collect::<Vec<_>>();
        assert!(reader.set_visible(room, &overflow).is_err());
        assert_eq!(
            reader.visible.len(),
            1,
            "invalid selection cannot partially replace demand"
        );
        reader.cancel();
        respond(&mut reader, old, AgentMessagePublicPhase::Publishing, 2);
        assert!(reader.progress(parent.0).is_none());
        assert!(reader.take_collab_requests().is_empty());
    }

    #[test]
    fn unavailable_and_stale_reads_preserve_last_truthful_milestone() {
        let mut reader = AgentMessageProgressReader::default();
        let parent = original(41);
        reader
            .set_visible(uuid::Uuid::from_u128(40), std::slice::from_ref(&parent))
            .unwrap();
        let now = crate::time::now();
        reader.refresh_at(now);
        let id = request(&mut reader);
        respond(&mut reader, id, AgentMessagePublicPhase::WaitingForReply, 2);
        reader.refresh_at(now + READ_INTERVAL);
        let id = request(&mut reader);
        reader.handle_collab_event(&CollabEvent::AgentMessageProgress {
            request_id: id,
            result: Err("unavailable".into()),
        });
        assert!(reader.unavailable(parent.0));
        assert_eq!(
            reader.progress(parent.0).unwrap().phase,
            AgentMessagePublicPhase::WaitingForReply
        );
        assert!(reader.request_refresh());
        let unchanged = reader
            .visible
            .get(&parent.0)
            .map(|entry| (entry.first_request, entry.reads, entry.suspended));
        assert!(!reader.request_refresh());
        assert_eq!(reader.queued.len(), 1);
        assert_eq!(
            reader.visible.get(&parent.0).map(|entry| (
                entry.first_request,
                entry.reads,
                entry.suspended
            )),
            unchanged,
            "a refused duplicate explicit refresh leaves its observation window untouched"
        );
        let id = request(&mut reader);
        respond(&mut reader, id, AgentMessagePublicPhase::Publishing, 1);
        assert_eq!(reader.progress(parent.0).unwrap().generation, 2);
        assert!(reader.unavailable(parent.0));
    }

    #[test]
    fn undrained_timeout_cannot_multiply_queue_and_terminal_stops_reads() {
        let mut reader = AgentMessageProgressReader::default();
        let parent = original(51);
        reader
            .set_visible(uuid::Uuid::from_u128(50), std::slice::from_ref(&parent))
            .unwrap();
        let now = crate::time::now();
        reader.refresh_at(now);
        assert_eq!(reader.queued.len(), 1);
        reader.refresh_at(now + READ_TIMEOUT);
        assert!(reader.queued.is_empty(), "timeout withdraws queued demand");
        assert!(reader.unavailable(parent.0));
        for attempt in 2..20 {
            reader.refresh_at(now + READ_TIMEOUT * attempt);
            assert!(
                reader.queued.is_empty(),
                "unknown never automatically issues another read"
            );
        }
        assert!(reader.request_refresh());
        let id = request(&mut reader);
        respond(
            &mut reader,
            id,
            AgentMessagePublicPhase::Committed {
                reply_row: original(52).1,
            },
            3,
        );
        reader.refresh_at(now + READ_TIMEOUT * 100);
        assert!(reader.take_collab_requests().is_empty());
        assert!(matches!(
            reader.progress(parent.0).unwrap().phase,
            AgentMessagePublicPhase::Committed { .. }
        ));
    }

    #[test]
    fn unchanged_waiting_observation_has_finite_window_and_explicit_recovery() {
        let mut reader = AgentMessageProgressReader::default();
        let parent = original(61);
        reader
            .set_visible(uuid::Uuid::from_u128(60), std::slice::from_ref(&parent))
            .unwrap();
        let now = crate::time::now();
        for attempt in 0..u32::from(MAX_AUTOMATIC_READS) {
            reader.refresh_at(now + READ_INTERVAL * attempt);
            let id = request(&mut reader);
            respond(&mut reader, id, AgentMessagePublicPhase::WaitingForReply, 1);
        }
        reader.refresh_at(now + OBSERVATION_WINDOW);
        assert!(reader.take_collab_requests().is_empty());
        assert!(reader.unavailable(parent.0));
        assert_eq!(
            reader.progress(parent.0).unwrap().phase,
            AgentMessagePublicPhase::WaitingForReply
        );
        assert!(reader.request_refresh());
        let id = request(&mut reader);
        respond(&mut reader, id, AgentMessagePublicPhase::Publishing, 2);
        assert!(!reader.unavailable(parent.0));
    }
}
