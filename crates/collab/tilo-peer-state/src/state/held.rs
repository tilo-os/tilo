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
    /// The earliest write of each author to each record that was let go
    /// unapplied, by its sequence. The author's later writes to the record
    /// wait for it: once a later one is applied, the earlier one never can
    /// be, its sequence behind the author's. Forgotten once it is applied
    /// or refused, never to make room: forgetting it would let a later
    /// write overtake it.
    behind: std::collections::BTreeMap<(Route, [u8; 32]), u64>,
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
    pub(super) fn remember_missed(&mut self, input: &DroppedInput) {
        if matches!(input.reason, DropReason::Refused(_)) {
            // Decided, not waiting: refused as any delivery of it would be.
            return;
        }
        let route = Route {
            app: input.app.clone(),
            record: input.record.clone(),
        };
        if !input.app.is_empty() {
            self.missed
                .behind
                .entry((route.clone(), input.author))
                .and_modify(|sequence| *sequence = (*sequence).min(input.sequence))
                .or_insert(input.sequence);
        }
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
                behind: std::mem::take(&mut self.missed.behind),
                ..Missed::default()
            };
        }
    }
    /// The sequence of `author`'s earlier write to `route` that was let go
    /// unapplied and that its write `sequence` would overtake. One applied
    /// since is forgotten.
    pub(super) fn overtakes_let_go(
        &mut self,
        route: &Route,
        author: [u8; 32],
        sequence: u64,
    ) -> Option<u64> {
        let key = (route.clone(), author);
        let earlier = *self.missed.behind.get(&key)?;
        let applied = self
            .core
            .documents
            .get(route)
            .and_then(|document| {
                document
                    .channel
                    .last_seq_per_author
                    .get(&collab_types::Ed25519PublicKey::from_bytes(author))
                    .copied()
            })
            .is_some_and(|last| last >= earlier);
        if applied {
            self.missed.behind.remove(&key);
            return None;
        }
        (sequence > earlier).then_some(earlier)
    }
    /// Forget `author`'s write `written` (its record and sequence) as one
    /// let go: the room refuses it, so nothing after it waits for it.
    pub(super) fn forget_refused(&mut self, author: [u8; 32], written: Option<&(Route, u64)>) {
        let Some((route, sequence)) = written else {
            return;
        };
        let key = (route.clone(), author);
        if self.missed.behind.get(&key) == Some(sequence) {
            self.missed.behind.remove(&key);
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
