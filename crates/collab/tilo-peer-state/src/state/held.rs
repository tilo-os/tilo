//! The prerequisite queue: app messages held while what they wait for is
//! still in flight, bounded, and never a refusal of the input being admitted.
use super::Route;
use super::{
    AdmissionError, AdmissionReport, DocumentAdmission, DroppedInput, InputDisposition,
    MissingPrerequisite, NeighbourMsg, ReceiveOutcome, StatePeer, SystemTime, VecDeque, err,
};
use crate::admission::DropReason;
use automerge::transaction::Transactable as _;

/// Most app messages one room holds for a prerequisite at once.
pub const MAX_HELD_INPUTS: usize = 64;
/// Most encoded bytes of app messages one room holds for a prerequisite.
pub const MAX_HELD_BYTES: usize = 4 * 1024 * 1024;

/// How a message is held: what it waits for, and the admission context it
/// is decided in again.
pub(super) struct Held {
    pub(super) retained_replay: bool,
    pub(super) prerequisite: MissingPrerequisite,
    pub(super) now: SystemTime,
}

/// Most distinct prerequisites [`Missed`] keeps apart; past them any change
/// that lands asks for a replay.
const MAX_MISSED: usize = 1024;

/// What the app messages this peer let go waited for. A message let go is
/// not lost: once something it waited for lands here, the room's host is
/// asked to replay what it retains ([`ReceiveOutcome::replay_wanted`]), and
/// the message is decided again. Not persisted: a device that restarts
/// subscribes afresh, and its host replays everything it retains anyway.
#[derive(Clone, Default)]
pub(super) struct Missed {
    /// Authors whose certificate was missing.
    authors: std::collections::BTreeSet<[u8; 32]>,
    /// Records whose checkpoint, coverage or earlier changes were missing,
    /// each as this peer held it then.
    routes: std::collections::BTreeMap<Route, Option<Vec<automerge::ChangeHash>>>,
    /// Something else was missing, or too much to tell apart: any change
    /// that lands asks for a replay.
    anything: bool,
}

impl Missed {
    fn is_empty(&self) -> bool {
        self.authors.is_empty() && self.routes.is_empty() && !self.anything
    }
}

/// A held message decided again when its place was needed.
enum Decided {
    Admitted(Vec<DocumentAdmission>),
    LetGo(DroppedInput),
}

fn dropped_input(
    author: &[u8; 32],
    message: &NeighbourMsg,
    waiting_for: Option<MissingPrerequisite>,
    reason: DropReason,
) -> DroppedInput {
    let (app, record, sequence) = match message {
        NeighbourMsg::AppDocument {
            app_id,
            record_id,
            envelope,
            ..
        } => (app_id.clone(), record_id.clone(), envelope.seq),
        _ => (String::new(), String::new(), 0),
    };
    DroppedInput {
        author: *author,
        app,
        record,
        sequence,
        waiting_for,
        reason,
    }
}

impl StatePeer {
    /// Hold `message` until `prerequisite` arrives: every later room change
    /// and admitted document retries what is held. The queue is bounded
    /// ([`MAX_HELD_INPUTS`], [`MAX_HELD_BYTES`]) and never refuses the input
    /// being admitted: when it is full, the message held longest is decided
    /// again and, when still undecidable, let go with what it waited for in
    /// the report's `dropped`. Nothing let go was applied, and the room's
    /// host still retains it.
    pub(super) fn defer(
        &mut self,
        source: [u8; 32],
        message: NeighbourMsg,
        held: Held,
    ) -> Result<AdmissionReport, AdmissionError> {
        let Held {
            retained_replay,
            prerequisite,
            now,
        } = held;
        let bytes = crate::postcard_sink::encoded_len(&message, usize::MAX).map_err(err)?;
        let deferred = |dropped| {
            AdmissionReport::new(
                InputDisposition::RetryLater(prerequisite.clone()),
                ReceiveOutcome {
                    deferred: true,
                    dropped,
                    ..Default::default()
                },
            )
        };
        if self.core.pending.iter().any(|(key, prior, replayed)| {
            key == &source && *prior == message && *replayed == retained_replay
        }) {
            return Ok(deferred(Vec::new()));
        }
        let full = |held: usize, held_bytes: usize| {
            held >= MAX_HELD_INPUTS || held_bytes.saturating_add(bytes) > MAX_HELD_BYTES
        };
        // A native publication cut exports no queue it changed
        // (`NativePublicationCut::finish`): its own message is refused.
        if self.core.native_cut_publisher.is_some()
            && full(self.core.pending.len(), self.core.pending_bytes)
        {
            return Err("app prerequisite queue full; fresh checkpoint required".into());
        }
        if bytes > MAX_HELD_BYTES {
            let input = dropped_input(
                &source,
                &message,
                Some(prerequisite.clone()),
                DropReason::TooLarge,
            );
            self.remember_missed(&input);
            let mut report = deferred(vec![input]);
            report.summary.deferred = false;
            return Ok(report);
        }
        let mut dropped = Vec::new();
        let mut settled = Vec::new();
        while full(self.core.pending.len(), self.core.pending_bytes) {
            let Some((author, oldest, replayed)) = self.core.pending.pop_front() else {
                break;
            };
            let size = crate::postcard_sink::encoded_len(&oldest, usize::MAX).map_err(err)?;
            self.core.pending_bytes = self.core.pending_bytes.saturating_sub(size);
            match self.decide_held(author, &oldest, replayed, now) {
                Decided::Admitted(admitted) => settled.extend(admitted),
                Decided::LetGo(input) => {
                    self.remember_missed(&input);
                    dropped.push(input);
                }
            }
        }
        self.core.pending_bytes = self.core.pending_bytes.saturating_add(bytes);
        self.core
            .pending
            .push_back((source, message, retained_replay));
        let mut report = deferred(dropped);
        report.settled.extend(settled);
        Ok(report)
    }
    /// Decide the held `message` again on a copy of this peer, holding
    /// nothing else: admitted there, the copy becomes this peer, keeping what
    /// is still held; otherwise it is let go with why.
    fn decide_held(
        &mut self,
        author: [u8; 32],
        message: &NeighbourMsg,
        replayed: bool,
        now: SystemTime,
    ) -> Decided {
        let mut probe = self.clone();
        probe.core.pending = VecDeque::new();
        probe.core.pending_bytes = 0;
        match probe.receive_with_context(author, message.clone(), now, replayed, false) {
            Ok(report) => {
                if let InputDisposition::RetryLater(waiting) = report.primary {
                    return Decided::LetGo(dropped_input(
                        &author,
                        message,
                        Some(waiting),
                        DropReason::Superseded,
                    ));
                }
                probe.core.pending = std::mem::take(&mut self.core.pending);
                probe.core.pending_bytes = self.core.pending_bytes;
                *self = probe;
                Decided::Admitted(report.settled)
            }
            Err(error) => Decided::LetGo(dropped_input(
                &author,
                message,
                None,
                DropReason::Refused(error.to_string()),
            )),
        }
    }
    /// Note what `input`, let go unapplied, waited for.
    fn remember_missed(&mut self, input: &DroppedInput) {
        if matches!(input.reason, DropReason::Refused(_)) {
            // Decided, not waiting: refused as any delivery of it would be.
            return;
        }
        let route = Route {
            app: input.app.clone(),
            record: input.record.clone(),
        };
        match input.waiting_for {
            Some(MissingPrerequisite::Certificate) => {
                self.missed.authors.insert(input.author);
            }
            Some(
                MissingPrerequisite::RecordCheckpoint
                | MissingPrerequisite::CheckpointCoverage
                | MissingPrerequisite::CausalDependencies,
            ) => {
                let held = self.route_heads(&route);
                self.missed.routes.entry(route).or_insert(held);
            }
            _ => self.missed.anything = true,
        }
        if self
            .missed
            .authors
            .len()
            .saturating_add(self.missed.routes.len())
            > MAX_MISSED
        {
            self.missed = Missed {
                anything: true,
                ..Missed::default()
            };
        }
    }
    fn route_heads(&self, route: &Route) -> Option<Vec<automerge::ChangeHash>> {
        self.core
            .documents
            .get(route)
            .map(|document| document.doc.base_heads())
    }
    /// Whether something a message let go waited for has landed since,
    /// forgetting what did: the room's host should replay what it retains.
    /// `landed` is whether the input just admitted changed anything.
    pub(super) fn missed_landed(&mut self, landed: bool) -> bool {
        if self.missed.is_empty() {
            return false;
        }
        let mut due = false;
        if self.missed.anything && landed {
            self.missed.anything = false;
            due = true;
        }
        let certificates = &self.core.certificates;
        let before = self.missed.authors.len();
        self.missed
            .authors
            .retain(|author| !certificates.contains_key(author));
        due |= self.missed.authors.len() != before;
        let routes = std::mem::take(&mut self.missed.routes);
        for (route, held) in routes {
            if self.route_heads(&route) == held {
                self.missed.routes.insert(route, held);
            } else {
                due = true;
            }
        }
        due
    }
}
