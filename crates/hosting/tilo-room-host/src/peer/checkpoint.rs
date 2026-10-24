//! Checkpoint prefixes: which buffered checkpoints may be retained
//! together, and the pending-replay work a retained batch leaves for
//! catching-up sessions.

use super::{
    Arc, Envelope, EnvelopeV2, Invariant, ReplayCatchup, SelectedPublication, Step, inbound_bytes,
    test_point,
};

pub(super) const RETENTION_PREFIX_FRAME_CAP: usize = 32;
pub(super) const RETENTION_PREFIX_BYTE_CAP: usize = 64 * 1024 * 1024;

pub(super) fn eligible_checkpoint_prefix<'a>(
    current: &Envelope,
    backlog: impl Iterator<Item = &'a EnvelopeV2>,
) -> usize {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    } = current
    else {
        return 1;
    };
    let Some((current_channel, crate::coedit::RetentionKind::Checkpoint)) =
        crate::coedit::retention_metadata(delta)
    else {
        return 1;
    };
    let current_bytes = delta.len().saturating_add(256);
    if current_bytes > RETENTION_PREFIX_BYTE_CAP {
        return 1;
    }
    let mut channels = std::collections::HashSet::new();
    channels.insert(current_channel);
    let mut total_bytes = current_bytes;
    let mut total = 1;
    for frame in backlog {
        if total == RETENTION_PREFIX_FRAME_CAP {
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id: frame_workspace,
            author_device_public_key: frame_author,
            delta: frame_delta,
            ..
        }) = frame
        else {
            break;
        };
        if frame_workspace != workspace_id || frame_author != author_device_public_key {
            break;
        }
        let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
            crate::coedit::retention_metadata(frame_delta)
        else {
            break;
        };
        if !channels.insert(channel) {
            break;
        }
        let frame_bytes = frame_delta.len().saturating_add(256);
        if total_bytes.saturating_add(frame_bytes) > RETENTION_PREFIX_BYTE_CAP {
            break;
        }
        total_bytes = total_bytes.saturating_add(frame_bytes);
        total = total.saturating_add(1);
    }
    total
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct BufferedRetentionPrefix {
    pub(super) frames: usize,
    pub(super) input_bytes: usize,
    pub(super) checkpoints: usize,
    pub(super) deltas: usize,
    pub(super) distinct_channels: usize,
    pub(super) frame_cap_reached: bool,
    pub(super) byte_cap_reached: bool,
    pub(super) stop_reason: &'static str,
}

pub(super) fn buffered_retention_prefix<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
) -> Option<BufferedRetentionPrefix> {
    buffered_retention_prefix_with_limits(
        frames,
        RETENTION_PREFIX_FRAME_CAP,
        RETENTION_PREFIX_BYTE_CAP,
    )
}

pub(super) fn buffered_retention_prefix_with_limits<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
    frame_cap: usize,
    byte_cap: usize,
) -> Option<BufferedRetentionPrefix> {
    let mut group: Option<(u64, [u8; 32])> = None;
    let mut channels = std::collections::HashSet::new();
    let mut inspected = false;
    let mut shape = BufferedRetentionPrefix {
        frames: 0,
        input_bytes: 0,
        checkpoints: 0,
        deltas: 0,
        distinct_channels: 0,
        frame_cap_reached: false,
        byte_cap_reached: false,
        stop_reason: "end",
    };
    for frame in frames {
        inspected = true;
        if shape.frames == frame_cap {
            shape.frame_cap_reached = true;
            shape.stop_reason = "frame_cap";
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        }) = frame
        else {
            shape.stop_reason = "non_delta";
            break;
        };
        let Some((channel, kind)) = crate::coedit::retention_metadata(delta) else {
            shape.stop_reason = "non_retained";
            break;
        };
        let current = (*workspace_id, *author_device_public_key);
        match group {
            Some(expected) if expected != current => {
                shape.stop_reason = if expected.0 == current.0 {
                    "author_change"
                } else {
                    "workspace_change"
                };
                break;
            }
            None => group = Some(current),
            _ => {}
        }
        let bytes = inbound_bytes(frame);
        if shape.input_bytes.saturating_add(bytes) > byte_cap {
            shape.byte_cap_reached = true;
            shape.stop_reason = "byte_cap";
            break;
        }
        shape.frames = shape.frames.saturating_add(1);
        shape.input_bytes = shape.input_bytes.saturating_add(bytes);
        channels.insert(channel);
        match kind {
            crate::coedit::RetentionKind::Checkpoint => {
                shape.checkpoints = shape.checkpoints.saturating_add(1);
            }
            crate::coedit::RetentionKind::Delta => shape.deltas = shape.deltas.saturating_add(1),
        }
    }
    if !inspected {
        return None;
    }
    if shape.frames == frame_cap && shape.stop_reason == "end" {
        shape.frame_cap_reached = true;
        shape.stop_reason = "frame_cap";
    }
    shape.distinct_channels = channels.len();
    Some(shape)
}

pub(super) fn retention_publication_kind(
    publication: &crate::store::shell_selection::Publication,
) -> &'static str {
    match publication {
        crate::store::shell_selection::Publication::Suppress => "suppress",
        crate::store::shell_selection::Publication::Activate(_) => "activate",
        crate::store::shell_selection::Publication::ReplayRequired(_) => "recovery",
        crate::store::shell_selection::Publication::Original => "original",
    }
}

#[derive(Clone)]
pub(super) struct ReplayPendingTarget {
    pub(super) device: [u8; 32],
    pub(super) live: Arc<std::sync::atomic::AtomicBool>,
    pub(super) catchup: Arc<ReplayCatchup>,
}
impl ReplayPendingTarget {
    fn active(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && self.catchup.phase.load(std::sync::atomic::Ordering::SeqCst) == ReplayCatchup::ACTIVE
    }
    pub(super) fn current(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        runtime
            .state_sessions
            .get(&self.device)
            .is_some_and(|session| {
                Arc::ptr_eq(&session.live, &self.live)
                    && session
                        .replay
                        .as_ref()
                        .is_some_and(|catchup| Arc::ptr_eq(catchup, &self.catchup))
            })
    }
}

enum ReplayPendingPublication {
    Original(Option<Arc<Envelope>>),
    Selected(crate::store::replay::PublicationCursor),
}
struct ReplayPendingFrame {
    bytes: Vec<u8>,
    _selected: Option<crate::store::replay::ReplayBatch>,
    sequence: Option<u64>,
    next_target: usize,
}
pub(super) struct ReplayPendingWork {
    continuity: Option<crate::store::continuity::Receipt>,
    publication: ReplayPendingPublication,
    targets: Vec<ReplayPendingTarget>,
    frame: Option<ReplayPendingFrame>,
    written: u64,
}

impl ReplayPendingWork {
    pub(super) fn original(
        envelope: Arc<Envelope>,
        targets: Vec<ReplayPendingTarget>,
        continuity: Option<crate::store::continuity::Receipt>,
    ) -> Self {
        Self {
            continuity,
            publication: ReplayPendingPublication::Original(Some(envelope)),
            targets,
            frame: None,
            written: 0,
        }
    }
    pub(super) fn selected(
        publication: &SelectedPublication,
        targets: Vec<ReplayPendingTarget>,
    ) -> Result<Self, crate::store::StoreError> {
        Ok(Self {
            continuity: None,
            publication: ReplayPendingPublication::Selected(publication.open()?),
            targets,
            frame: None,
            written: 0,
        })
    }
    pub(super) fn step(
        &mut self,
        state: &mut crate::persistence::State,
        budget: &crate::store::replay::ReplayBudget,
    ) -> Result<Step<u64>, crate::store::StoreError> {
        for _ in 0..crate::store::bounded::RUN_ENTRIES {
            if self.frame.is_none() {
                let (bytes, selected) = match &mut self.publication {
                    ReplayPendingPublication::Original(envelope) => envelope
                        .take()
                        .map(|envelope| crate::transport::encode(&envelope))
                        .transpose()
                        .map(|bytes| (bytes, None))
                        .map_err(|error| crate::persistence::error(error.to_string()))?,
                    ReplayPendingPublication::Selected(cursor) => match cursor.step(budget)? {
                        Step::More => continue,
                        Step::Done(Some(batch)) => {
                            let bytes = crate::transport::encode(
                                batch.front().invariant("one activation frame")?,
                            )
                            .map_err(|error| crate::persistence::error(error.to_string()))?;
                            (Some(bytes), Some(batch))
                        }
                        Step::Done(None) => (None, None),
                    },
                };
                let Some(bytes) = bytes else {
                    return Ok(Step::Done(self.written));
                };
                self.frame = Some(ReplayPendingFrame {
                    bytes,
                    _selected: selected,
                    sequence: None,
                    next_target: 0,
                });
            }
            let frame = self.frame.as_mut().invariant("frame missing")?;
            let Some(target) = self.targets.get(frame.next_target) else {
                self.frame = None;
                continue;
            };
            frame.next_target = frame.next_target.saturating_add(1);
            if !target.active() {
                continue;
            }
            let sequence = if let Some(sequence) = frame.sequence {
                sequence
            } else {
                let sequence = match &self.continuity {
                    Some(receipt) => receipt.sequence,
                    None => state.next_sequence()?,
                };
                frame.sequence = Some(sequence);
                sequence
            };
            if let Some(receipt) = &self.continuity {
                state
                    .store
                    .enqueue_continuity_pending(&target.device, &frame.bytes, receipt)?;
            } else {
                // Not the device's durable queue: one fsynced file per
                // catching-up session for each commit made a crowd joining
                // at once write the room's every change once per newcomer.
                let holding = crate::timing::start();
                target.catchup.hold(sequence, &frame.bytes)?;
                crate::timing::window("state.held_write", holding);
            }
            self.written = self.written.saturating_add(frame.bytes.len() as u64);
        }
        test_point!(if self.frame.is_some()
            => pause(state.store.root(), "replay-pending-frame-held"));
        Ok(Step::More)
    }
}
