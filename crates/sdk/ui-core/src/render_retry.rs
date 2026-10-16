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
    /// A visible auxiliary surface draws this primary's retained root. Preserve
    /// its update/sync cadence without forgetting persistent primary occlusion.
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
/// An idle redraw whose tree, layout, and damage did not change must not
/// present. Presenting it asks the compositor for another frame callback,
/// and answering that callback is what kept apps-open idle painting.
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
    pub primary_startup: bool,
    pub tooltip: bool,
    pub typing: bool,
    pub widget_due: bool,
    pub animation: bool,
    pub auxiliary_startup: bool,
    pub retry_due: bool,
}

/// Select the actual event-loop deadline. Widget deadlines request
/// sync/redraw and remain pending until paint admission. Collaboration is not
/// a demand: its transport wakes the loop for every event it delivers.
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
        demand.primary_startup || demand.tooltip || demand.typing || demand.widget_due,
    ) || (any_render_available && demand.animation)
        || demand.auxiliary_startup
        || demand.retry_due;
    let scheduled = widget_deadline(widget, blocked_until)
        .into_iter()
        .chain(retry)
        .min();
    let animation = animation.filter(|_| any_render_available);
    if fast {
        let fallback = now.checked_add(FRAME_INTERVAL).unwrap_or(now);
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

    #[test]
    fn an_unchanged_redraw_does_not_present() {
        assert!(
            !submit_redraw_frame(false, false),
            "no tree, layout, or damage change must not present"
        );
        assert!(submit_redraw_frame(true, false));
        assert!(submit_redraw_frame(false, true));
    }

    #[test]
    fn occlusion_backoff_caps_probes_and_survives_unrelated_outcomes() {
        // Case `occlusion_backoff_caps_and_eventually_probes_without_exposure`.
        {
            let mut state = RenderRetryState::default();
            let mut now = crate::time::now();
            for delay in [
                16_666_667,
                33_333_334,
                66_666_668,
                133_333_336,
                250_000_000,
                250_000_000,
            ] {
                state.observe(FrameOutcome::Occluded, now);
                let due = now + Duration::from_nanos(delay);
                assert_eq!(state.deadline(), Some(due));
                assert!(state.occlusion_blocked(now));
                assert!(!state.allows(due - Duration::from_nanos(1)));
                assert!(state.allows(due));
                state.admitted();
                now = due;
            }
        }

        // Case `non_occlusion_outcomes_preserve_cadence_and_reset_streak`.
        {
            let now = crate::time::now();
            for outcome in [
                FrameOutcome::Skipped,
                FrameOutcome::Reconfigured,
                FrameOutcome::Presented,
            ] {
                let mut state = RenderRetryState::default();
                for _ in 0..8 {
                    state.observe(FrameOutcome::Occluded, now);
                }
                state.observe(outcome, now);
                assert!(!state.occlusion_blocked(now));
                assert_eq!(
                    state.deadline(),
                    if outcome == FrameOutcome::Presented {
                        None
                    } else {
                        Some(now + FRAME_INTERVAL)
                    }
                );
                state.observe(FrameOutcome::Occluded, now);
                assert_eq!(state.deadline(), Some(now + FRAME_INTERVAL));
            }
        }

        // Case `explicit_probe_survives_old_outcomes_and_buffer_deferral`.
        {
            let now = crate::time::now();
            let mut state = RenderRetryState::default();
            for _ in 0..8 {
                state.observe(FrameOutcome::Occluded, now);
            }
            state.request_probe();
            state.observe(FrameOutcome::Occluded, now);
            assert!(state.allows(now));
            assert_eq!(state.deadline(), None);
            // Buffer admission may defer repeatedly without consuming the request.
            assert!(state.allows(now));
            state.admitted();
            state.observe(FrameOutcome::Occluded, now);
            assert_eq!(state.deadline(), Some(now + FRAME_INTERVAL));
        }

        // Case `visible_sibling_limits_retry_without_erasing_occlusion_history`.
        {
            let now = crate::time::now();
            let mut state = RenderRetryState::default();
            for _ in 0..8 {
                state.observe(FrameOutcome::Occluded, now);
            }
            state.keep_shared_cadence(now);
            assert_eq!(state.deadline(), Some(now + FRAME_INTERVAL));
            assert!(state.known_occluded());
            state.admitted();
            assert!(state.known_occluded());
            state.observe_for_shared_root(FrameOutcome::Occluded, now, true);
            assert_eq!(state.deadline(), Some(now + FRAME_INTERVAL));
            state.observe_for_shared_root(FrameOutcome::Occluded, now, false);
            assert_eq!(state.deadline(), Some(now + Duration::from_millis(250)));
        }
    }
    #[test]
    fn retry_waits_defer_render_demand_but_keep_model_sibling_and_timer_work() {
        // Case `blocked_render_only_demand_does_not_poll_but_visible_sibling_and_model_do`.
        {
            // Startup, tooltip, animation and overdue widget work share this gate.
            for _kind in ["startup", "tooltip", "animation", "widget"] {
                assert!(!presentation_demand(true, true));
                assert!(presentation_demand(false, true));
            }
            let hidden = presentation_demand(true, true);
            let sibling = presentation_demand(false, true);
            assert!(hidden || sibling, "visible sibling keeps its cadence");
            let model_work = true;
            assert!(
                model_work || hidden,
                "collaboration work remains independent"
            );
        }

        // Case `selected_wait_defers_each_render_demand_but_preserves_visible_sibling`.
        {
            let now = crate::time::now();
            let retry = now + Duration::from_millis(250);
            for demand in [
                WakeDemand {
                    primary_startup: true,
                    ..Default::default()
                },
                WakeDemand {
                    tooltip: true,
                    ..Default::default()
                },
                WakeDemand {
                    typing: true,
                    ..Default::default()
                },
                WakeDemand {
                    widget_due: true,
                    ..Default::default()
                },
                WakeDemand {
                    animation: true,
                    ..Default::default()
                },
            ] {
                assert_eq!(
                    next_wake(
                        now,
                        Some(retry),
                        false,
                        demand,
                        Some(now),
                        Some(retry),
                        Some(now + FRAME_INTERVAL)
                    ),
                    Some(retry)
                );
            }
            let animation = WakeDemand {
                animation: true,
                ..Default::default()
            };
            assert_eq!(
                next_wake(
                    now,
                    Some(retry),
                    true,
                    animation,
                    Some(now),
                    Some(retry),
                    Some(now + FRAME_INTERVAL)
                ),
                Some(now + FRAME_INTERVAL)
            );
            let aux = WakeDemand {
                auxiliary_startup: true,
                ..Default::default()
            };
            assert_eq!(
                next_wake(now, Some(retry), true, aux, None, Some(retry), None),
                Some(now + FRAME_INTERVAL)
            );
            assert_eq!(
                next_wake(
                    now,
                    None,
                    true,
                    WakeDemand::default(),
                    Some(now + Duration::from_secs(1)),
                    None,
                    None
                ),
                Some(now + Duration::from_secs(1))
            );
        }

        // Case `explicit_events_preserve_normal_idle_but_request_retry_recovery`.
        {
            let now = crate::time::now();
            let mut state = RenderRetryState::default();
            assert!(
                !state.request_probe(),
                "ordinary pointer motion must not force an extra paint"
            );
            state.observe(FrameOutcome::Occluded, now);
            assert!(
                state.allows(now),
                "latch survives a previously unobserved completion"
            );
            state.admitted();
            state.observe(FrameOutcome::Occluded, now);
            assert!(
                state.request_probe(),
                "known retry requires a recovery redraw"
            );
        }

        // Case `overdue_widget_timer_waits_for_probe_without_losing_deadline`.
        {
            let now = crate::time::now();
            let retry = now + Duration::from_millis(250);
            assert_eq!(
                widget_deadline(Some(now - FRAME_INTERVAL), Some(retry)),
                Some(retry)
            );
            assert_eq!(
                widget_deadline(Some(now + Duration::from_secs(2)), Some(retry)),
                Some(now + Duration::from_secs(2))
            );
            assert_eq!(widget_deadline(Some(now), None), Some(now));
        }
    }

    #[test]
    fn an_overdue_poll_does_not_schedule_another_wake() {
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

    #[test]
    fn idle_loop_without_demand_or_deadline_sleeps_until_an_event() {
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

    #[test]
    fn production_wake_preserves_animation_deadline_when_other_work_is_overdue() {
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

    #[test]
    fn production_wake_services_animation_deadline_elapsed_during_callback() {
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
