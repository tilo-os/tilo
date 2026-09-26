//! Reading a trace in replay order.
//!
//! Replay is following a past session (`docs/architecture/feedback.md`): a
//! replay peer publishes the starting snapshot, then the timeline, and the
//! viewer's app follows it. This module is the part of that which needs no
//! toolkit and no transport:
//!
//! - [`steps`]: where a person would want to stop. A step is an
//!   `Interaction` reaching a terminal phase (the attempt ended, whether it
//!   worked or not) or a participant's View changing.
//! - [`position_at`]: the structural state at an offset, straight from the
//!   trace (latest View per participant, latest inspect keyframe per route,
//!   latest phase per interaction).
//! - [`Playhead`]: which timeline entries are due as replay time passes, and
//!   what a seek has to do. Automerge cannot rewind a document that already
//!   merged later changes, so a backward seek is [`Seek::Reload`]: start
//!   again from the snapshot and fast-forward.

use std::time::Duration;

use tilo_actions::{InteractionPhase, InvocationId, SemanticInteraction};

use crate::{Event, InspectKeyframe, Offset, PresenceChange, Route, SessionTrace};

/// Where a replay can stop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// Position in [`steps`]' result.
    pub index: usize,
    pub at: Offset,
    /// Index into [`SessionTrace::timeline`] of the entry that ends the step.
    pub timeline: usize,
    pub kind: StepKind,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepKind {
    /// An attempt ended: committed, failed, cancelled or rejected.
    Interaction {
        participant: String,
        invocation: InvocationId,
        phase: InteractionPhase,
    },
    /// A participant's View (scroll, view mode, open overlays) changed.
    View { participant: String },
}

/// Every step of `trace`, in timeline order.
pub fn steps(trace: &SessionTrace) -> Vec<Step> {
    let mut out = Vec::new();
    for (timeline, entry) in trace.timeline.iter().enumerate() {
        let Event::Presence(update) = &entry.event else {
            continue;
        };
        let kind = match &update.change {
            PresenceChange::Interaction(interaction) if interaction.phase.is_terminal() => {
                StepKind::Interaction {
                    participant: update.participant.clone(),
                    invocation: interaction.invocation,
                    phase: interaction.phase.clone(),
                }
            }
            PresenceChange::View => StepKind::View {
                participant: update.participant.clone(),
            },
            _ => continue,
        };
        out.push(Step {
            index: out.len(),
            at: entry.at,
            timeline,
            kind,
        });
    }
    out
}

/// The structural state of a trace at one offset.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Position<'a> {
    pub at: Offset,
    /// Latest View per participant: `(participant, scope)`, where `scope` is
    /// the SDK `PresenceKey::View` wire encoding carrying the subset.
    pub views: Vec<(&'a str, &'a [u8])>,
    /// Latest inspect keyframe per route.
    pub keyframes: Vec<&'a InspectKeyframe>,
    /// Latest phase per invocation.
    pub interactions: Vec<&'a SemanticInteraction>,
}

/// What the trace says at `at`: every entry at or before it applied.
pub fn position_at(trace: &SessionTrace, at: Offset) -> Position<'_> {
    let mut position = Position {
        at,
        ..Position::default()
    };
    for entry in trace.timeline.iter().take_while(|entry| entry.at <= at) {
        match &entry.event {
            Event::Presence(update) => match &update.change {
                PresenceChange::View => {
                    let view = (update.participant.as_str(), update.scope.as_slice());
                    match position
                        .views
                        .iter_mut()
                        .find(|(participant, _)| *participant == update.participant)
                    {
                        Some(slot) => *slot = view,
                        None => position.views.push(view),
                    }
                }
                PresenceChange::Interaction(interaction) => {
                    match position
                        .interactions
                        .iter_mut()
                        .find(|known| known.invocation == interaction.invocation)
                    {
                        Some(slot) => *slot = interaction,
                        None => position.interactions.push(interaction),
                    }
                }
                _ => {}
            },
            Event::Inspect(keyframe) => {
                match position
                    .keyframes
                    .iter_mut()
                    .find(|known| known.route == keyframe.route)
                {
                    Some(slot) => *slot = keyframe,
                    None => position.keyframes.push(keyframe),
                }
            }
            Event::Op(_) => {}
        }
    }
    position
}

/// Replay speed, as a multiple of recorded time.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Speed(u32);

impl Speed {
    /// Recorded time.
    pub const NORMAL: Self = Self(1_000);
    const MIN_PERMILLE: u32 = 125;
    const MAX_PERMILLE: u32 = 16_000;

    /// A speed of `factor` × recorded time, clamped to 1/8…16.
    pub fn from_factor(factor: f64) -> Self {
        let permille = (factor * 1_000.0).round();
        if !permille.is_finite() || permille <= f64::from(Self::MIN_PERMILLE) {
            return Self(Self::MIN_PERMILLE);
        }
        if permille >= f64::from(Self::MAX_PERMILLE) {
            return Self(Self::MAX_PERMILLE);
        }
        // In range and finite: the conversion is exact.
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to [125, 16000] above"
        )]
        Self(permille as u32)
    }

    pub fn factor(self) -> f64 {
        f64::from(self.0) / 1_000.0
    }

    fn scale(self, elapsed: Duration) -> Duration {
        let micros = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        Duration::from_micros(micros.saturating_mul(u64::from(self.0)) / 1_000)
    }
}

impl Default for Speed {
    fn default() -> Self {
        Self::NORMAL
    }
}

/// What a seek asks of the replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Seek {
    /// Publish these timeline entries, in order, now.
    Forward(std::ops::Range<usize>),
    /// The target is behind what was already published: load the starting
    /// snapshot into a fresh replay and publish `0..end`.
    Reload { end: usize },
}

/// The replay clock over one trace: how far it has published, where in
/// recorded time it stands, and whether it is running.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Playhead {
    /// Timeline entries `0..published` have been published.
    published: usize,
    at: Offset,
    speed: Speed,
    playing: bool,
}

impl Playhead {
    /// Paused at the start, nothing published yet.
    pub fn new() -> Self {
        Self {
            published: 0,
            at: Offset::default(),
            speed: Speed::NORMAL,
            playing: false,
        }
    }

    pub fn published(&self) -> usize {
        self.published
    }

    pub fn at(&self) -> Offset {
        self.at
    }

    pub fn speed(&self) -> Speed {
        self.speed
    }

    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn set_speed(&mut self, speed: Speed) {
        self.speed = speed;
    }

    pub fn play(&mut self) {
        self.playing = true;
    }

    pub fn pause(&mut self) {
        self.playing = false;
    }

    /// Advance recorded time by `elapsed` wall time at the current speed and
    /// return the entries that became due. Nothing moves while paused. At
    /// the end of the trace the playhead pauses itself.
    pub fn tick(&mut self, trace: &SessionTrace, elapsed: Duration) -> std::ops::Range<usize> {
        if !self.playing {
            return self.published..self.published;
        }
        let advanced = self
            .at
            .as_duration()
            .saturating_add(self.speed.scale(elapsed));
        self.at = Offset::from_duration(advanced).min(trace.duration.max(self.at));
        let start = self.published;
        let end = start.saturating_add(
            trace
                .timeline
                .get(start..)
                .unwrap_or_default()
                .iter()
                .take_while(|entry| entry.at <= self.at)
                .count(),
        );
        self.published = end;
        if end >= trace.timeline.len() && self.at >= trace.duration {
            self.playing = false;
        }
        start..end
    }

    /// Move so that exactly the entries `0..end` are published, standing at
    /// the offset of the last one. The caller performs the returned seek.
    pub fn seek_to_entry(&mut self, trace: &SessionTrace, end: usize) -> Seek {
        let end = end.min(trace.timeline.len());
        let seek = if end >= self.published {
            Seek::Forward(self.published..end)
        } else {
            Seek::Reload { end }
        };
        self.published = end;
        self.at = end
            .checked_sub(1)
            .and_then(|last| trace.timeline.get(last))
            .map_or_else(Offset::default, |entry| entry.at);
        seek
    }

    /// Seek to the end of `step`.
    pub fn seek_to_step(&mut self, trace: &SessionTrace, step: &Step) -> Seek {
        self.seek_to_entry(trace, step.timeline.saturating_add(1))
    }

    /// The step that ends at or before the playhead, if any.
    pub fn current_step<'a>(&self, steps: &'a [Step]) -> Option<&'a Step> {
        steps
            .iter()
            .rev()
            .find(|step| step.timeline < self.published)
    }

    /// The first step that ends after the playhead.
    pub fn next_step<'a>(&self, steps: &'a [Step]) -> Option<&'a Step> {
        steps.iter().find(|step| step.timeline >= self.published)
    }

    /// The step before the current one, or `None` when the current one is
    /// the first (or there is none).
    pub fn previous_step<'a>(&self, steps: &'a [Step]) -> Option<&'a Step> {
        let current = self.current_step(steps)?;
        current
            .index
            .checked_sub(1)
            .and_then(|index| steps.get(index))
    }
}

impl Default for Playhead {
    fn default() -> Self {
        Self::new()
    }
}

/// The recorded routes of one app, starting snapshot first.
pub fn routes_of_app<'a>(trace: &'a SessionTrace, app_id: &str) -> Vec<&'a Route> {
    let mut routes: Vec<&Route> = trace
        .snapshots
        .iter()
        .map(|snapshot| &snapshot.route)
        .chain(
            trace
                .timeline
                .iter()
                .filter_map(|entry| match &entry.event {
                    Event::Op(op) => Some(&op.route),
                    _ => None,
                }),
        )
        .filter(|route| route.app_id == app_id)
        .collect();
    routes.sort();
    routes.dedup();
    routes
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::{incremental, interaction, presence, route, snapshot};
    use crate::{DocumentOp, Origin, RouteSnapshot, SnapshotPoint, TimedEvent};

    fn trace() -> SessionTrace {
        let (started, failed) = interaction();
        let op = |at: u64, seq: u64| TimedEvent {
            at: Offset(at),
            event: Event::Op(DocumentOp {
                route: route("so.tilo.notes"),
                from: "alice".into(),
                envelope: incremental(seq),
            }),
        };
        let at = |at: u64, change: PresenceChange| TimedEvent {
            at: Offset(at),
            event: Event::Presence(presence("alice", change)),
        };
        SessionTrace {
            origin: Origin::Explicit,
            started_unix_ms: 0,
            duration: Offset(1_000_000),
            environment: crate::Environment::default(),
            snapshots: vec![RouteSnapshot {
                route: route("so.tilo.notes"),
                from: "alice".into(),
                at: Offset(0),
                point: SnapshotPoint::Start,
                frames: vec![snapshot(1)],
            }],
            timeline: vec![
                op(100_000, 2),
                at(200_000, PresenceChange::View),
                at(300_000, PresenceChange::Interaction(Box::new(started))),
                op(400_000, 3),
                at(500_000, PresenceChange::Interaction(Box::new(failed))),
                at(600_000, PresenceChange::Caret { pos: 4 }),
            ],
            tags: Vec::new(),
            layer: crate::SessionLayer::Logic,
            redactions: crate::Redactions::default(),
            truncated: false,
        }
    }

    #[test]
    fn steps_are_view_changes_and_terminal_interactions() {
        let steps = steps(&trace());
        assert_eq!(steps.len(), 2);
        assert!(matches!(&steps[0].kind, StepKind::View { participant } if participant == "alice"));
        assert_eq!(steps[0].timeline, 1);
        assert!(matches!(
            &steps[1].kind,
            StepKind::Interaction {
                phase: InteractionPhase::Failed { .. },
                ..
            }
        ));
        assert_eq!(steps[1].at, Offset(500_000));
    }

    #[test]
    fn position_keeps_the_latest_view_and_phase() {
        let trace = trace();
        let early = position_at(&trace, Offset(350_000));
        assert_eq!(early.views, vec![("alice", [1_u8, 0, 3].as_slice())]);
        assert!(!early.interactions[0].phase.is_terminal());
        let late = position_at(&trace, trace.duration);
        assert_eq!(late.interactions.len(), 1);
        assert!(late.interactions[0].phase.is_unsuccessful());
    }

    #[test]
    fn playing_releases_entries_as_recorded_time_passes() {
        let trace = trace();
        let mut playhead = Playhead::new();
        assert_eq!(
            playhead.tick(&trace, Duration::from_secs(1)),
            0..0,
            "paused"
        );
        playhead.play();
        assert_eq!(playhead.tick(&trace, Duration::from_millis(250)), 0..2);
        playhead.set_speed(Speed::from_factor(2.0));
        assert_eq!(playhead.tick(&trace, Duration::from_millis(100)), 2..4);
        assert_eq!(playhead.at(), Offset(450_000));
        assert_eq!(playhead.tick(&trace, Duration::from_secs(5)), 4..6);
        assert!(!playhead.is_playing(), "the end of the trace pauses");
        assert_eq!(playhead.at(), trace.duration);
    }

    #[test]
    fn seeking_back_reloads_and_forward_fast_forwards() {
        let trace = trace();
        let steps = steps(&trace);
        let mut playhead = Playhead::new();
        assert_eq!(
            playhead.seek_to_step(&trace, &steps[1]),
            Seek::Forward(0..5)
        );
        assert_eq!(playhead.current_step(&steps), Some(&steps[1]));
        assert_eq!(playhead.previous_step(&steps), Some(&steps[0]));
        assert_eq!(playhead.next_step(&steps), None);
        assert_eq!(
            playhead.seek_to_step(&trace, &steps[0]),
            Seek::Reload { end: 2 }
        );
        assert_eq!(playhead.at(), Offset(200_000));
        assert_eq!(playhead.next_step(&steps), Some(&steps[1]));
    }

    #[test]
    fn speed_is_clamped() {
        assert_eq!(Speed::from_factor(100.0), Speed::from_factor(16.0));
        assert_eq!(Speed::from_factor(0.0), Speed::from_factor(0.125));
        assert_eq!(Speed::from_factor(f64::NAN), Speed::from_factor(0.125));
        assert!(Speed::from_factor(0.5) < Speed::NORMAL);
    }
}
