//! When this device's presence leaves: one schedule for everything it says.
//!
//! A person moves their pointer at the display's rate and types at the
//! keyboard's, so what changes is collected and leaves whole: a pointer that
//! moved five times between two sends leaves once, at the place it reached.
//! The ceiling is the relay's, not the display's: nothing leaves sooner than
//! [`MIN_INTERVAL_MS`] after the last burst, whatever the input rate.
//!
//! After the last change the burst is said twice — a datagram lost is a
//! pointer that stops, and the repeat also settles a pointer that stopped —
//! and then restated slowly, so someone whose session came up after the first
//! word still hears it.

use std::collections::VecDeque;

use collab_types::presence::{Presence, PresenceKey};

/// The soonest two bursts of one device leave: 120 Hz, the relay's ceiling.
pub const MIN_INTERVAL_MS: f64 = 1000.0 / 120.0;
/// Loss repair after the last change: the first repeat settles a stopped
/// pointer quickly, the second bridges one lost frame.
pub const REPEATS_MS: [f64; 2] = [20.0, 200.0];
/// A slow restatement while nothing changes, far below the rate ceiling.
pub const KEEPALIVE_MS: f64 = 3_000.0;

/// The part of what a device says a queued frame carries: a frame for the
/// same part replaces the one queued, so a burst says each part once.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Part {
    Person,
    Profile,
    Arrived,
    Availability,
    Status,
    Following,
    /// A wave, by its count: a new wave is a new wave, never a replacement
    /// for the one before it.
    Wave(u32),
    Pointer,
    /// An app's frame under its own key.
    App(PresenceKey),
}

impl Part {
    fn of(frame: &Presence) -> Self {
        match &frame.payload {
            collab_types::presence::Payload::Person { .. } => Self::Person,
            collab_types::presence::Payload::Profile { .. } => Self::Profile,
            collab_types::presence::Payload::Arrived => Self::Arrived,
            collab_types::presence::Payload::Availability { .. } => Self::Availability,
            collab_types::presence::Payload::Status { .. } => Self::Status,
            collab_types::presence::Payload::Following { .. } => Self::Following,
            collab_types::presence::Payload::Wave { seq, .. } => Self::Wave(*seq),
            collab_types::presence::Payload::Pointer { .. }
            | collab_types::presence::Payload::Left
                if matches!(frame.key, PresenceKey::Pointer { .. }) =>
            {
                Self::Pointer
            }
            _ => Self::App(frame.key.clone()),
        }
    }
}

/// The frames waiting to leave, and when the next ones may.
#[derive(Clone, Debug, Default)]
pub(crate) struct Schedule {
    seq: u32,
    queued: VecDeque<Presence>,
    last_sent_ms: Option<f64>,
    /// The last burst, said again for loss repair.
    repeat: Vec<Presence>,
    repeat_at: Option<f64>,
    repeat_index: usize,
}

impl Schedule {
    /// Queue `frame`: what this device changed, to leave as soon as the
    /// ceiling allows.
    pub(super) fn push(&mut self, frame: Presence) {
        let part = Part::of(&frame);
        if let Some(queued) = self
            .queued
            .iter_mut()
            .find(|queued| Part::of(queued) == part)
        {
            *queued = frame;
        } else {
            self.queued.push_back(frame);
        }
    }

    fn waiting(&self) -> bool {
        !self.queued.is_empty()
    }

    /// The burst due at `now_ms`, numbered as the track numbers its objects.
    /// One at most per call.
    pub(super) fn due(&mut self, now_ms: f64) -> Option<Vec<(u32, Presence)>> {
        if let Some(last) = self.last_sent_ms
            && now_ms - last < MIN_INTERVAL_MS
        {
            return None;
        }
        if self.waiting() {
            let burst: Vec<Presence> = self.queued.drain(..).collect();
            self.repeat.clone_from(&burst);
            self.repeat_index = 0;
            self.repeat_at = Some(now_ms + REPEATS_MS[0]);
            return Some(self.numbered(burst, now_ms));
        }
        if let Some(at) = self.repeat_at
            && now_ms >= at
            && let Some(just) = REPEATS_MS.get(self.repeat_index).copied()
        {
            let next = REPEATS_MS.get(self.repeat_index.saturating_add(1)).copied();
            self.repeat_index = self.repeat_index.saturating_add(1);
            self.repeat_at = next.map(|repeat| now_ms + (repeat - just));
            let burst = self.repeat.clone();
            return Some(self.numbered(burst, now_ms));
        }
        None
    }

    fn numbered(&mut self, burst: Vec<Presence>, now_ms: f64) -> Vec<(u32, Presence)> {
        self.last_sent_ms = Some(now_ms);
        burst
            .into_iter()
            .map(|frame| {
                self.seq = self.seq.wrapping_add(1);
                (self.seq, frame)
            })
            .collect()
    }

    /// Every queued frame now, whatever the ceiling: a join says the whole
    /// of what a device is at once.
    pub(super) fn flush(&mut self, now_ms: f64) -> Vec<(u32, Presence)> {
        if !self.waiting() {
            return Vec::new();
        }
        let burst: Vec<Presence> = self.queued.drain(..).collect();
        self.repeat.clone_from(&burst);
        self.repeat_index = 0;
        self.repeat_at = Some(now_ms + REPEATS_MS[0]);
        self.numbered(burst, now_ms)
    }

    /// When the next burst is due, if one is: what a loop sleeps until.
    pub(super) fn next_due(&self, now_ms: f64) -> Option<f64> {
        if self.waiting() {
            let at = self
                .last_sent_ms
                .map_or(now_ms, |last| last + MIN_INTERVAL_MS);
            return Some(at.max(now_ms));
        }
        self.repeat_at.filter(|at| *at > now_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::presence::Payload;

    fn said(status: &str) -> Presence {
        Presence::desktop(
            "w1",
            Payload::Status {
                text: status.to_owned(),
                automatic: false,
            },
        )
    }

    #[test]
    fn a_burst_says_each_part_once_and_the_last_word_wins() {
        let mut schedule = Schedule::default();
        schedule.push(said("one"));
        schedule.push(said("two"));
        schedule.push(said("three"));
        let burst = schedule.due(0.0).expect("due at once");
        assert_eq!(burst.len(), 1, "one part, said once");
        assert_eq!(
            burst[0].1.payload,
            Payload::Status {
                text: "three".to_owned(),
                automatic: false
            }
        );
        assert_eq!(burst[0].0, 1, "the track numbers its objects");
    }

    #[test]
    fn nothing_leaves_twice_within_the_ceiling() {
        let mut schedule = Schedule::default();
        schedule.push(said("one"));
        assert!(schedule.due(0.0).is_some());
        schedule.push(said("two"));
        assert!(schedule.due(1.0).is_none(), "inside the ceiling");
        let burst = schedule.due(MIN_INTERVAL_MS).expect("due at the ceiling");
        assert_eq!(burst[0].0, 2, "one object per burst");
    }

    #[test]
    fn a_change_is_said_again_for_a_link_that_lost_it() {
        let mut schedule = Schedule::default();
        schedule.push(said("one"));
        assert!(schedule.due(0.0).is_some());
        let first = schedule.due(REPEATS_MS[0]).expect("the first repeat");
        assert_eq!(first[0].1, said("one"));
        assert!(schedule.due(REPEATS_MS[0] + 1.0).is_none(), "not yet");
        assert!(schedule.due(REPEATS_MS[1]).is_some(), "the second repeat");
        assert!(
            schedule.due(KEEPALIVE_MS).is_none(),
            "then rest until the next change"
        );
        assert!(schedule.next_due(KEEPALIVE_MS).is_none(), "nothing is due");
    }

    #[test]
    fn a_wave_is_a_new_wave_each_time() {
        let mut schedule = Schedule::default();
        schedule.push(Presence::desktop(
            "w1",
            Payload::Wave {
                seq: 1,
                target: "ada".into(),
            },
        ));
        schedule.push(Presence::desktop(
            "w1",
            Payload::Wave {
                seq: 2,
                target: "ada".into(),
            },
        ));
        let burst = schedule.due(0.0).expect("due at once");
        assert_eq!(burst.len(), 2, "each wave is its own wave");
    }
}
