//! Keeping the window on screen when its graphics are lost.
//!
//! A present can fail with the window's EGL surface or context gone: the host compositor restarted
//! or reset the GPU, the driver refused the surface, the desktop was busy. None of that ends the
//! session. The window drops its graphics and makes them again on the same window, a frame later
//! at first and then less often ([`backoff`]), while the session goes on serving its clients, its
//! door and its agent. A frame that is shown again ends the episode and the schedule starts over;
//! after [`GIVE_UP_AFTER`] rebuilds in a row came to nothing the window is given up and the session
//! runs on without it.
//!
//! [`Recovery`] is the decision alone (what a frame's outcome leads to, and when), and [`frame`]
//! drives one frame of a [`Painter`] through it: the window's real graphics in the backend, a
//! scripted one in the tests.

use std::time::{Duration, Instant};

/// The wait before each rebuild of an episode, by how many rebuilds of it already failed: the first
/// a frame later, then longer, and every two seconds from the fifth on.
const BACKOFF: [Duration; 5] = [
    Duration::from_millis(16),
    Duration::from_millis(50),
    Duration::from_millis(200),
    Duration::from_secs(1),
    Duration::from_secs(2),
];

/// Rebuilds in a row that may fail before the window is given up. With [`BACKOFF`] that is a little
/// over a minute after the loss: long enough to outlast a host compositor restarting or a GPU
/// reset, after which a window still without graphics is not coming back by waiting.
pub(crate) const GIVE_UP_AFTER: u32 = 34;

/// The wait before the next rebuild, once `failures` rebuilds of the episode failed.
pub(crate) fn backoff(failures: u32) -> Duration {
    let [.., last] = BACKOFF;
    usize::try_from(failures)
        .ok()
        .and_then(|failures| BACKOFF.get(failures))
        .copied()
        .unwrap_or(last)
}

/// When the next rebuild is due, `failures` failed rebuilds into an episode at `now`.
fn due(now: Instant, failures: u32) -> Instant {
    now.checked_add(backoff(failures)).unwrap_or(now)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Frames reach the window.
    Showing,
    /// The window's graphics were lost: `failures` rebuilds since came to nothing, and the next is
    /// due at `due`. `rebuilt` while a rebuild succeeded and its first frame is still to be shown.
    Lost {
        failures: u32,
        due: Instant,
        rebuilt: bool,
    },
    /// Given up: nothing is drawn to the window again.
    Unusable,
}

/// What the window does at a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    /// Draw the frame and present it.
    Present,
    /// Make the window's graphics again, then present.
    Rebuild,
    /// Nothing until the next rebuild is due, at this instant.
    Wait(Instant),
    /// The window is given up.
    Unusable,
}

/// A change worth one line in the log: an episode is told once as it starts and once as it ends,
/// not at every frame of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Note {
    /// The window's graphics were lost; rebuilding them.
    Lost,
    /// A frame reached the window again, after this many rebuilds.
    Recovered { rebuilds: u32 },
    /// Given up after this many rebuilds.
    Unusable { rebuilds: u32 },
}

/// Where the window stands between its graphics being lost and shown again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Recovery {
    phase: Phase,
}

impl Default for Recovery {
    fn default() -> Self {
        Self {
            phase: Phase::Showing,
        }
    }
}

impl Recovery {
    /// What the window does at a frame at `now`.
    pub(crate) fn step(&self, now: Instant) -> Step {
        match self.phase {
            Phase::Showing | Phase::Lost { rebuilt: true, .. } => Step::Present,
            Phase::Lost { due, .. } if now < due => Step::Wait(due),
            Phase::Lost { .. } => Step::Rebuild,
            Phase::Unusable => Step::Unusable,
        }
    }

    /// A frame reached the window: whatever was lost is back, and a later loss starts the schedule
    /// over.
    pub(crate) fn presented(&mut self) -> Option<Note> {
        match self.phase {
            Phase::Showing | Phase::Unusable => None,
            Phase::Lost { failures, .. } => {
                self.phase = Phase::Showing;
                Some(Note::Recovered {
                    rebuilds: failures.saturating_add(1),
                })
            }
        }
    }

    /// A present at `now` failed. On a window that was showing, an episode starts; on graphics
    /// just rebuilt, the rebuild did not hold and counts as failed.
    pub(crate) fn present_failed(&mut self, now: Instant) -> Option<Note> {
        match self.phase {
            Phase::Showing => {
                self.phase = Phase::Lost {
                    failures: 0,
                    due: due(now, 0),
                    rebuilt: false,
                };
                Some(Note::Lost)
            }
            Phase::Lost { failures, .. } => self.failed(failures.saturating_add(1), now),
            Phase::Unusable => None,
        }
    }

    /// The window's graphics were made again: the next frame tries them.
    pub(crate) fn rebuilt(&mut self) {
        if let Phase::Lost { rebuilt, .. } = &mut self.phase {
            *rebuilt = true;
        }
    }

    /// Making the window's graphics again failed at `now`.
    pub(crate) fn rebuild_failed(&mut self, now: Instant) -> Option<Note> {
        match self.phase {
            Phase::Lost { failures, .. } => self.failed(failures.saturating_add(1), now),
            Phase::Showing | Phase::Unusable => None,
        }
    }

    fn failed(&mut self, failures: u32, now: Instant) -> Option<Note> {
        if failures >= GIVE_UP_AFTER {
            self.phase = Phase::Unusable;
            return Some(Note::Unusable { rebuilds: failures });
        }
        self.phase = Phase::Lost {
            failures,
            due: due(now, failures),
            rebuilt: false,
        };
        None
    }
}

/// The window's graphics, as [`frame`] drives them.
pub(crate) trait Painter {
    /// Draw the frame and hand it to the host.
    fn present(&mut self) -> Result<(), String>;
    /// Drop the window's graphics and make them again on the same window.
    fn rebuild(&mut self) -> Result<(), String>;
    /// Drop the window's graphics for good and take the window off the desktop.
    fn give_up(&mut self);
}

/// What one frame of the window came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Frame {
    /// The frame reached the window.
    Presented,
    /// It did not: the window's graphics are lost, and a rebuild is due at this instant.
    Lost(Instant),
    /// The window is given up.
    Unusable,
}

/// Draw one frame at `now` on `painter`, rebuilding its graphics first when a rebuild is due, and
/// log what changed.
pub(crate) fn frame(recovery: &mut Recovery, painter: &mut impl Painter, now: Instant) -> Frame {
    match recovery.step(now) {
        Step::Unusable => return Frame::Unusable,
        Step::Wait(due) => return Frame::Lost(due),
        Step::Rebuild => match painter.rebuild() {
            Ok(()) => recovery.rebuilt(),
            Err(error) => {
                let note = recovery.rebuild_failed(now);
                return settle(recovery, painter, note, &error, now);
            }
        },
        Step::Present => {}
    }
    match painter.present() {
        Ok(()) => {
            if let Some(Note::Recovered { rebuilds }) = recovery.presented() {
                tracing::info!(rebuilds, "the window is drawn again");
            }
            Frame::Presented
        }
        Err(error) => {
            let note = recovery.present_failed(now);
            settle(recovery, painter, note, &error, now)
        }
    }
}

/// After a failure: log the note it made and give the window up if that is what it came to.
fn settle(
    recovery: &Recovery,
    painter: &mut impl Painter,
    note: Option<Note>,
    error: &str,
    now: Instant,
) -> Frame {
    match note {
        Some(Note::Lost) => {
            tracing::warn!(error, "the window's graphics were lost; rebuilding them")
        }
        Some(Note::Unusable { rebuilds }) => {
            tracing::error!(
                error,
                rebuilds,
                "the window's graphics could not be made again; the session runs on without its window"
            );
            painter.give_up();
            return Frame::Unusable;
        }
        Some(Note::Recovered { .. }) | None => {
            tracing::debug!(error, "the window's graphics are still lost");
        }
    }
    match recovery.step(now) {
        Step::Wait(due) => Frame::Lost(due),
        Step::Unusable => Frame::Unusable,
        Step::Present | Step::Rebuild => Frame::Lost(now),
    }
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    clippy::unchecked_time_subtraction,
    clippy::unwrap_used,
    reason = "test timelines a minute from now"
)]
mod tests {
    use super::*;

    #[test]
    fn a_window_that_shows_presents() {
        let recovery = Recovery::default();
        assert_eq!(recovery.step(Instant::now()), Step::Present);
    }

    #[test]
    fn the_first_failure_starts_an_episode_and_rebuilds_a_frame_later() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        assert_eq!(recovery.present_failed(start), Some(Note::Lost));
        let due = start + backoff(0);
        assert_eq!(recovery.step(start), Step::Wait(due));
        assert_eq!(
            recovery.step(due - Duration::from_millis(1)),
            Step::Wait(due)
        );
        assert_eq!(recovery.step(due), Step::Rebuild);
        assert_eq!(backoff(0), Duration::from_millis(16));
    }

    #[test]
    fn a_rebuild_is_tried_with_a_frame_and_a_shown_frame_ends_the_episode() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        recovery.present_failed(start);
        recovery.rebuilt();
        assert_eq!(recovery.step(start), Step::Present);
        assert_eq!(recovery.presented(), Some(Note::Recovered { rebuilds: 1 }));
        assert_eq!(recovery, Recovery::default());
        // A frame that shows on a window that never lost anything says nothing.
        assert_eq!(recovery.presented(), None);
    }

    #[test]
    fn failed_rebuilds_wait_longer_each_time_and_then_every_two_seconds() {
        let mut now = Instant::now();
        let mut recovery = Recovery::default();
        recovery.present_failed(now);
        let mut waits = Vec::new();
        for _ in 0..8 {
            let Step::Wait(due) = recovery.step(now) else {
                panic!("an episode waits for its next rebuild");
            };
            waits.push(due - now);
            now = due;
            assert_eq!(recovery.step(now), Step::Rebuild);
            assert_eq!(recovery.rebuild_failed(now), None, "told once per episode");
        }
        let ms = Duration::from_millis;
        assert_eq!(
            waits,
            [
                ms(16),
                ms(50),
                ms(200),
                ms(1000),
                ms(2000),
                ms(2000),
                ms(2000),
                ms(2000)
            ]
        );
    }

    /// Graphics that were made again but fail at their first frame did not come back: that counts
    /// as a failed rebuild, and the schedule goes on rather than starting over.
    #[test]
    fn a_rebuild_whose_frame_fails_counts_as_failed() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        recovery.present_failed(start);
        recovery.rebuilt();
        assert_eq!(recovery.present_failed(start), None);
        assert_eq!(recovery.step(start), Step::Wait(start + backoff(1)));
        recovery.rebuilt();
        assert_eq!(recovery.present_failed(start), None);
        assert_eq!(recovery.step(start), Step::Wait(start + backoff(2)));
    }

    #[test]
    fn the_window_is_given_up_after_a_minute_of_failed_rebuilds() {
        let lost = Instant::now();
        let mut now = lost;
        let mut recovery = Recovery::default();
        recovery.present_failed(now);
        let mut rebuilds = 0;
        let note = loop {
            let Step::Wait(due) = recovery.step(now) else {
                panic!("still recovering after {rebuilds} rebuilds");
            };
            now = due;
            rebuilds += 1;
            if let Some(note) = recovery.rebuild_failed(now) {
                break note;
            }
        };
        assert_eq!(
            note,
            Note::Unusable {
                rebuilds: GIVE_UP_AFTER
            }
        );
        assert_eq!(rebuilds, GIVE_UP_AFTER);
        let tried_for = now - lost;
        assert!(
            tried_for >= Duration::from_secs(60) && tried_for < Duration::from_secs(65),
            "given up after {tried_for:?}"
        );
        assert_eq!(recovery.step(now), Step::Unusable);
        // Nothing brings a given-up window back or tells of it again.
        assert_eq!(recovery.present_failed(now), None);
        assert_eq!(recovery.rebuild_failed(now), None);
        recovery.rebuilt();
        assert_eq!(recovery.step(now + Duration::from_secs(10)), Step::Unusable);
    }

    #[test]
    fn a_shown_frame_resets_the_backoff_for_the_next_loss() {
        let mut now = Instant::now();
        let mut recovery = Recovery::default();
        recovery.present_failed(now);
        for _ in 0..5 {
            let Step::Wait(due) = recovery.step(now) else {
                panic!("an episode waits");
            };
            now = due;
            recovery.rebuild_failed(now);
        }
        assert_eq!(recovery.step(now), Step::Wait(now + backoff(5)));
        now += backoff(5);
        recovery.rebuilt();
        assert_eq!(recovery.presented(), Some(Note::Recovered { rebuilds: 6 }));
        // A new loss is a new episode: told again, and rebuilt a frame later.
        assert_eq!(recovery.present_failed(now), Some(Note::Lost));
        assert_eq!(recovery.step(now), Step::Wait(now + backoff(0)));
    }

    #[test]
    fn backoff_past_the_schedule_stays_at_its_last_step() {
        assert_eq!(backoff(4), Duration::from_secs(2));
        assert_eq!(backoff(1000), Duration::from_secs(2));
        assert_eq!(backoff(u32::MAX), Duration::from_secs(2));
    }

    /// What a scripted window does at each call, and what it was asked.
    #[derive(Default)]
    struct Scripted {
        presents: Vec<Result<(), String>>,
        rebuilds: Vec<Result<(), String>>,
        calls: Vec<&'static str>,
    }

    impl Painter for Scripted {
        fn present(&mut self) -> Result<(), String> {
            self.calls.push("present");
            self.presents.remove(0)
        }
        fn rebuild(&mut self) -> Result<(), String> {
            self.calls.push("rebuild");
            self.rebuilds.remove(0)
        }
        fn give_up(&mut self) {
            self.calls.push("give up");
        }
    }

    fn lost() -> Result<(), String> {
        Err("ContextLost(EGLCreateSurface(BadAlloc))".into())
    }

    /// The backend's frame path on a window whose surface is lost: the failed frame is not shown,
    /// the next frames before the rebuild is due draw nothing, the rebuild comes with a frame on the
    /// new graphics, and the frames after are drawn as before.
    #[test]
    fn a_lost_window_is_rebuilt_and_drawn_again() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        let mut window = Scripted {
            presents: vec![Ok(()), lost(), Ok(()), Ok(())],
            rebuilds: vec![Ok(())],
            ..Scripted::default()
        };
        assert_eq!(frame(&mut recovery, &mut window, start), Frame::Presented);
        let due = start + backoff(0);
        assert_eq!(frame(&mut recovery, &mut window, start), Frame::Lost(due));
        let early = start + Duration::from_millis(5);
        assert_eq!(frame(&mut recovery, &mut window, early), Frame::Lost(due));
        assert_eq!(frame(&mut recovery, &mut window, due), Frame::Presented);
        assert_eq!(frame(&mut recovery, &mut window, due), Frame::Presented);
        assert_eq!(
            window.calls,
            ["present", "present", "rebuild", "present", "present"]
        );
    }

    #[test]
    fn a_window_that_cannot_be_rebuilt_is_given_up_once() {
        let mut now = Instant::now();
        let mut recovery = Recovery::default();
        let mut window = Scripted {
            presents: vec![lost()],
            rebuilds: (0..GIVE_UP_AFTER).map(|_| lost()).collect(),
            ..Scripted::default()
        };
        let mut outcome = frame(&mut recovery, &mut window, now);
        while let Frame::Lost(due) = outcome {
            now = due;
            outcome = frame(&mut recovery, &mut window, now);
        }
        assert_eq!(outcome, Frame::Unusable);
        assert_eq!(
            frame(&mut recovery, &mut window, now + Duration::from_secs(60)),
            Frame::Unusable
        );
        let rebuilds = window
            .calls
            .iter()
            .filter(|call| **call == "rebuild")
            .count();
        assert_eq!(rebuilds, usize::try_from(GIVE_UP_AFTER).unwrap());
        assert_eq!(window.calls.first(), Some(&"present"));
        assert_eq!(window.calls.last(), Some(&"give up"));
        assert_eq!(
            window
                .calls
                .iter()
                .filter(|call| **call == "give up")
                .count(),
            1
        );
    }

    /// Rebuilt graphics that fail again at once are rebuilt again on the schedule, and a frame that
    /// finally shows leaves the window drawing as before.
    #[test]
    fn a_rebuild_that_does_not_hold_is_tried_again() {
        let start = Instant::now();
        let mut recovery = Recovery::default();
        let mut window = Scripted {
            presents: vec![lost(), lost(), Ok(())],
            rebuilds: vec![Ok(()), Err("BadAlloc".into()), Ok(())],
            ..Scripted::default()
        };
        let Frame::Lost(first) = frame(&mut recovery, &mut window, start) else {
            panic!("lost");
        };
        let Frame::Lost(second) = frame(&mut recovery, &mut window, first) else {
            panic!("the rebuilt surface failed at once");
        };
        assert_eq!(second, first + backoff(1));
        let Frame::Lost(third) = frame(&mut recovery, &mut window, second) else {
            panic!("the second rebuild failed");
        };
        assert_eq!(third, second + backoff(2));
        assert_eq!(frame(&mut recovery, &mut window, third), Frame::Presented);
        assert_eq!(recovery, Recovery::default());
        assert_eq!(
            window.calls,
            [
                "present", "rebuild", "present", "rebuild", "rebuild", "present"
            ]
        );
    }
}
