//! Paces animation-only redraw requests; input and renderer recovery bypass it.
use crate::clock::{Duration, Instant};

const FRAME_INTERVAL: Duration = Duration::from_nanos(16_666_667);

#[derive(Default)]
pub(super) struct AnimationSchedule {
    next_tick: Option<Instant>,
}

impl AnimationSchedule {
    /// Returns whether to request a frame and the next animation wake deadline.
    pub(super) fn poll(&mut self, now: Instant, active: bool) -> (bool, Option<Instant>) {
        if !active {
            self.next_tick = None;
            return (false, None);
        }
        let due = self.next_tick.is_none_or(|deadline| now >= deadline);
        if due {
            // Anchor only after a due request. Unrelated callbacks must neither
            // redraw early nor keep pushing the animation deadline forward.
            self.next_tick = Some(now.checked_add(FRAME_INTERVAL).unwrap_or(now));
        }
        (due, self.next_tick)
    }
}

/// Choose the fast-pump wake without allowing overdue model/retry work to
/// conceal the independently paced animation deadline.
pub(super) fn next_wake(
    now: Instant,
    fallback: Instant,
    scheduled: Option<Instant>,
    animation: Option<Instant>,
) -> Instant {
    let wake = scheduled
        .filter(|deadline| *deadline > now)
        .map_or(fallback, |deadline| deadline.min(fallback));
    // Unlike due model/retry work, this deadline may have elapsed *after*
    // poll without being served. Preserve it as an immediate wake.
    animation.map_or(wake, |deadline| wake.min(deadline.max(now)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn animation_schedule_repeated_callbacks_do_not_redraw_or_postpone_tick() {
        let start = Instant::now();
        let mut schedule = AnimationSchedule::default();
        let deadline = start + Duration::from_nanos(16_666_667);
        assert_eq!(schedule.poll(start, true), (true, Some(deadline)));
        for millis in [3, 6, 9, 16] {
            assert_eq!(
                schedule.poll(start + Duration::from_millis(millis), true),
                (false, Some(deadline))
            );
        }
        assert_eq!(
            schedule.poll(deadline, true),
            (true, Some(deadline + FRAME_INTERVAL))
        );
        assert_eq!(
            schedule.poll(deadline, true),
            (false, Some(deadline + FRAME_INTERVAL))
        );
    }

    #[test]
    fn animation_schedule_idle_or_reduced_motion_resets_and_reactivation_is_immediate() {
        let start = Instant::now();
        let mut schedule = AnimationSchedule::default();
        assert!(schedule.poll(start, true).0);
        assert_eq!(
            schedule.poll(start + Duration::from_millis(1), false),
            (false, None)
        );
        assert_eq!(
            schedule.poll(start + Duration::from_millis(2), false),
            (false, None)
        );
        let resumed = start + Duration::from_millis(3);
        assert_eq!(
            schedule.poll(resumed, true),
            (true, Some(resumed + FRAME_INTERVAL))
        );
    }

    #[test]
    fn animation_schedule_late_callback_emits_one_frame_without_catchup_burst() {
        let start = Instant::now();
        let mut schedule = AnimationSchedule::default();
        schedule.poll(start, true);
        let late = start + Duration::from_millis(100);
        assert_eq!(
            schedule.poll(late, true),
            (true, Some(late + FRAME_INTERVAL))
        );
        assert_eq!(
            schedule.poll(late, true),
            (false, Some(late + FRAME_INTERVAL))
        );
    }
    #[test]
    fn animation_schedule_overdue_model_deadline_does_not_hide_animation_wake() {
        let now = Instant::now();
        let animation = now + Duration::from_millis(4);
        assert_eq!(
            next_wake(
                now,
                now + FRAME_INTERVAL,
                Some(now - Duration::from_millis(1)),
                Some(animation)
            ),
            animation
        );
    }

    #[test]
    fn animation_schedule_deadline_elapsed_during_callback_wakes_immediately() {
        let now = Instant::now();
        assert_eq!(
            next_wake(
                now,
                now + FRAME_INTERVAL,
                None,
                Some(now - Duration::from_millis(1))
            ),
            now
        );
    }
}
