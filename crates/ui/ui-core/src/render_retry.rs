//! Per-surface retry admission. Explicit interaction survives queued outcomes.
#[cfg(feature = "native")]
use super::FrameOutcome;
#[cfg(feature = "native")]
use crate::time::Duration;
#[cfg(feature = "native")]
use crate::time::Instant;

#[cfg(feature = "native")]
pub(crate) const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);
#[derive(Default)]
pub(crate) struct RenderRetryState {
    #[cfg(feature = "native")]
    deadline: Option<Instant>,
    #[cfg(feature = "native")]
    occlusions: u8,
    #[cfg(feature = "native")]
    probe: bool,
}
impl RenderRetryState {
    #[cfg(feature = "native")]
    pub(crate) fn observe(&mut self, outcome: FrameOutcome, now: Instant) {
        self.occlusions = if outcome == FrameOutcome::Occluded {
            self.occlusions.saturating_add(1)
        } else {
            0
        };
        self.deadline = match outcome {
            FrameOutcome::Presented | FrameOutcome::SurfaceGone => None,
            _ => {
                let delay = if self.occlusions > 0 {
                    FRAME_INTERVAL
                        .saturating_mul(1_u32 << self.occlusions.saturating_sub(1).min(4))
                        .min(Duration::from_millis(250))
                } else {
                    FRAME_INTERVAL
                };
                Some(now.checked_add(delay).unwrap_or(now))
            }
        };
    }
    #[cfg(feature = "native")]
    pub(crate) fn known_occluded(&self) -> bool {
        self.occlusions > 0 && !self.probe
    }
    /// A visible auxiliary surface draws this primary's retained root.
    #[cfg(feature = "native")]
    pub(crate) fn keep_shared_cadence(&mut self, now: Instant) {
        if self.known_occluded() {
            let soon = now.checked_add(FRAME_INTERVAL).unwrap_or(now);
            self.deadline = self.deadline.map(|deadline| deadline.min(soon));
        }
    }
    #[cfg(feature = "native")]
    pub(crate) fn observe_for_shared_root(
        &mut self,
        outcome: FrameOutcome,
        now: Instant,
        visible_sibling: bool,
    ) {
        self.observe(outcome, now);
        if visible_sibling {
            self.keep_shared_cadence(now);
        }
    }
    #[cfg(feature = "native")]
    pub(crate) fn request_probe(&mut self) -> bool {
        let recovering = self.deadline.is_some() || self.occlusions > 0;
        self.probe = true;
        recovering
    }
    #[cfg(feature = "native")]
    pub(crate) fn allows(&self, now: Instant) -> bool {
        self.probe || self.deadline.is_none_or(|deadline| deadline <= now)
    }
    #[cfg(feature = "native")]
    pub(crate) fn admitted(&mut self) {
        if self.probe {
            self.occlusions = 0;
        }
        self.probe = false;
        self.deadline = None;
    }
    #[cfg(feature = "native")]
    pub(crate) fn deadline(&self) -> Option<Instant> {
        if self.probe { None } else { self.deadline }
    }
    #[cfg(feature = "native")]
    pub(crate) fn occlusion_blocked(&self, now: Instant) -> bool {
        self.occlusions > 0 && !self.allows(now)
    }
}
/// How often a window that takes no frame runs the passes a frame would: idle enough to cost
/// nothing, prompt enough to answer work queued while no frame comes.
#[cfg(feature = "native")]
pub(crate) const UNDRAWN_TICK_INTERVAL: Duration = Duration::from_millis(50);

/// When a window that takes no frame runs its state passes ([`crate::App::run_state_tick`]): one
/// that is occluded, or one whose last frame's callback has not come, as a hidden surface hears
/// none. State never waits on presentation: an invocation the session or an agent sent is
/// answered whether or not anyone sees the window.
#[derive(Default)]
#[cfg(feature = "native")]
pub(crate) struct UndrawnTicks {
    frame_owed: bool,
    at: Option<Instant>,
}

#[cfg(feature = "native")]
impl UndrawnTicks {
    /// A frame went out, and the windowing system holds the next one back until its callback.
    pub(crate) fn presented(&mut self) {
        self.frame_owed = true;
    }

    /// The window took a frame.
    pub(crate) fn drew(&mut self) {
        self.frame_owed = false;
    }

    /// Whether the passes run now, for a window that is `occluded` or owed a frame with state
    /// `pending`. The first runs an interval after the window stopped taking frames, so a frame
    /// that comes in time runs them as it always does.
    pub(crate) fn due(&mut self, occluded: bool, pending: bool, now: Instant) -> bool {
        if !(pending && (occluded || self.frame_owed)) {
            self.at = None;
            return false;
        }
        let due = self.at.is_some_and(|at| at <= now);
        if due || self.at.is_none() {
            self.at = Some(now.checked_add(UNDRAWN_TICK_INTERVAL).unwrap_or(now));
        }
        due
    }

    /// Whether a frame went out and the display has not shown it within a
    /// tick: it is not pacing this window, and what a person typed need not
    /// wait for its callback. Occluded windows are left to their state
    /// passes, as they have no callback to wait for.
    pub(crate) fn owed_past_tick(&mut self, occluded: bool, now: Instant) -> bool {
        self.frame_owed && !occluded && self.due(occluded, true, now)
    }

    /// When the loop wakes for the next pass.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.at
    }
}

/// An idle redraw whose tree, layout, and damage did not change must not present.
#[cfg(feature = "native")]
pub(crate) fn submit_redraw_frame(full_redraw: bool, has_damage: bool) -> bool {
    full_redraw || has_damage
}

/// Whether paint-only work warrants the fast pump for this surface.
#[cfg(feature = "native")]
pub(crate) fn presentation_demand(blocked: bool, demand: bool) -> bool {
    demand && !blocked
}

/// Defer a widget's draw-serviced timer while its surface cannot draw.
#[cfg(feature = "native")]
pub(crate) fn widget_deadline(
    deadline: Option<Instant>,
    blocked_until: Option<Instant>,
) -> Option<Instant> {
    deadline.map(|deadline| blocked_until.map_or(deadline, |retry| deadline.max(retry)))
}

#[derive(Default, Clone, Copy)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent concurrent wake sources may be active together; an exclusive enum would lose demand"
)]
#[cfg(feature = "native")]
pub(crate) struct WakeDemand {
    pub tooltip: bool,
    pub typing: bool,
    pub widget_due: bool,
    pub animation: bool,
    pub auxiliary_startup: bool,
    pub retry_due: bool,
    /// One refresh of the window's output: how soon a fast demand without its own deadline wakes again.
    pub frame_interval: Duration,
}

/// Select the actual event-loop deadline.
#[cfg(feature = "native")]
pub(crate) fn next_wake(
    now: Instant,
    blocked_until: Option<Instant>,
    any_render_available: bool,
    demand: WakeDemand,
    widget: Option<Instant>,
    retry: Option<Instant>,
    animation: Option<Instant>,
) -> Option<Instant> {
    let fast = presentation_demand(
        blocked_until.is_some(),
        demand.tooltip || demand.typing || demand.widget_due,
    ) || (any_render_available && demand.animation)
        || demand.auxiliary_startup
        || demand.retry_due;
    let scheduled = widget_deadline(widget, blocked_until)
        .into_iter()
        .chain(retry)
        .min();
    let animation = animation.filter(|_| any_render_available);
    if fast {
        let interval = if demand.frame_interval.is_zero() {
            FRAME_INTERVAL
        } else {
            demand.frame_interval
        };
        let fallback = now.checked_add(interval).unwrap_or(now);
        let wake = scheduled
            .filter(|deadline| *deadline > now)
            .map_or(fallback, |deadline| deadline.min(fallback));
        Some(animation.map_or(wake, |deadline| wake.min(deadline.max(now))))
    } else {
        // A deadline that is already due was the poll this callback just
        // ran. Parking on it would return immediately and keep the process
        // scheduled after the tree, layout, and animations stayed put.
        scheduled.filter(|deadline| *deadline > now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_retry_backs_off_and_the_loop_wakes_only_for_demand;

        phase an_unchanged_redraw_does_not_present;
        {
            assert!(
                !submit_redraw_frame(false, false),
                "no tree, layout, or damage change must not present"
            );
            assert!(submit_redraw_frame(true, false));
            assert!(submit_redraw_frame(false, true));
        }

        phase a_frame_owed_past_a_tick_is_painted_without_the_displays_callback;
        // A display shows a frame at its next refresh; one that has not
        // shown the last frame within a tick is not pacing this window, and
        // what the person typed need not wait for its callback.
        {
            let now = crate::time::now();
            let mut owed = UndrawnTicks::default();
            assert!(!owed.owed_past_tick(false, now), "no frame has gone out");
            owed.presented();
            assert!(
                !owed.owed_past_tick(false, now),
                "the callback may still come in time"
            );
            assert!(
                !owed.owed_past_tick(true, now + UNDRAWN_TICK_INTERVAL),
                "an occluded window waits for its own passes, not a frame"
            );
            assert!(
                owed.owed_past_tick(false, now + UNDRAWN_TICK_INTERVAL),
                "a display that has not shown the frame within a tick does not pace the next"
            );
            assert!(
                !owed.owed_past_tick(false, now + UNDRAWN_TICK_INTERVAL),
                "one frame past the tick, not one on every loop turn"
            );
        }

        phase a_fast_demand_wakes_on_the_outputs_refresh;
        {
            let now = crate::time::now();
            let beat = Duration::from_nanos(11_111_358);
            let demand = WakeDemand {
                typing: true,
                frame_interval: beat,
                ..Default::default()
            };
            assert_eq!(
                next_wake(now, None, true, demand, None, None, None),
                Some(now + beat),
                "a 90 Hz output wakes every 11.1 ms, not every 16.7 ms"
            );
        }

        phase an_overdue_poll_does_not_schedule_another_wake;
        {
            let now = crate::time::now();
            assert_eq!(
                next_wake(
                    now,
                    None,
                    true,
                    WakeDemand::default(),
                    Some(now - Duration::from_millis(1)),
                    None,
                    None,
                ),
                None,
                "a poll that already ran must not wake again when nothing changed"
            );
        }

        phase idle_loop_without_demand_or_deadline_sleeps_until_an_event;
        {
            let now = crate::time::now();
            assert_eq!(
                next_wake(now, None, true, WakeDemand::default(), None, None, None),
                None
            );
            assert_eq!(
                next_wake(
                    now,
                    None,
                    true,
                    WakeDemand::default(),
                    None,
                    None,
                    Some(now + FRAME_INTERVAL)
                ),
                None,
                "an animation deadline without animation demand is not a wake"
            );
        }

        phase production_wake_preserves_animation_deadline_when_other_work_is_overdue;
        {
            let now = crate::time::now();
            let animation = now + Duration::from_millis(4);
            assert_eq!(
                next_wake(
                    now,
                    None,
                    true,
                    WakeDemand {
                        animation: true,
                        ..Default::default()
                    },
                    Some(now - Duration::from_millis(1)),
                    None,
                    Some(animation),
                ),
                Some(animation)
            );
        }

        phase production_wake_services_animation_deadline_elapsed_during_callback;
        {
            let now = crate::time::now();
            assert_eq!(
                next_wake(
                    now,
                    None,
                    true,
                    WakeDemand {
                        animation: true,
                        ..Default::default()
                    },
                    None,
                    None,
                    Some(now - Duration::from_millis(1)),
                ),
                Some(now)
            );
        }
    }
}
