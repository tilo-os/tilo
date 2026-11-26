//! What a key holder does about a fire time, given the progress the
//! workspace keeps: on time once, a missed time once when the trigger catches
//! up (never once per period), and a moment once however late.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "in a test these are the assertion"
)]

use super::*;
use crate::CatchUp;

const ZONE: TimeZone = TimeZone::UTC;

fn every(catch_up: CatchUp) -> Timed<'static> {
    // The schedule lives in a `static` so the borrowed value outlives the call.
    static MINUTE: Schedule = Schedule::Interval { seconds: 60 };
    Timed::Repeating {
        schedule: &MINUTE,
        catch_up,
    }
}

#[test]
fn a_first_fire_time_starts_the_schedule_and_nothing_fires_yet() {
    assert_eq!(
        plan(every(CatchUp::Skip), None, 3_600_001, &ZONE),
        SchedulePlan::Waiting {
            answered_ms: 3_600_001
        }
    );
    // From there the next time is a minute away.
    assert_eq!(
        plan(every(CatchUp::Skip), Some(3_600_001), 3_600_001, &ZONE),
        SchedulePlan::Waiting {
            answered_ms: 3_600_001
        }
    );
}

#[test]
fn a_fire_time_comes_once_and_then_the_next() {
    assert_eq!(
        plan(every(CatchUp::Skip), Some(3_600_001), 3_660_000, &ZONE),
        SchedulePlan::Fire {
            due_ms: 3_660_000,
            answered_ms: 3_660_000,
            missed: false
        }
    );
    // The progress moved past the fire time even when it ran late.
    assert_eq!(
        plan(every(CatchUp::Skip), Some(3_660_001), 3_660_500, &ZONE),
        SchedulePlan::Waiting {
            answered_ms: 3_660_001
        }
    );
}

#[test]
fn a_missed_fire_time_is_caught_up_once_and_skipped_when_the_trigger_skips() {
    let answered = 3_660_000;
    let ten_periods_later = answered + 10 * 60_000;
    assert!(ten_periods_later - 3_720_000 > MISSED_AFTER_MS);
    let caught_up = plan(
        every(CatchUp::RunOnce),
        Some(answered),
        ten_periods_later,
        &ZONE,
    );
    let SchedulePlan::Fire {
        due_ms,
        answered_ms,
        missed,
    } = caught_up
    else {
        panic!("a trigger that catches up fires the missed time once")
    };
    assert!(missed);
    assert_eq!(due_ms, 3_720_000, "the first fire time after the answer");
    assert_eq!(answered_ms, ten_periods_later);
    // One fire, not ten: the next time is the first after the progress.
    assert_eq!(
        plan(
            every(CatchUp::RunOnce),
            Some(answered_ms),
            ten_periods_later,
            &ZONE
        ),
        SchedulePlan::Waiting {
            answered_ms: ten_periods_later
        }
    );
    // A trigger that skips missed times records the progress and never fires
    // them.
    assert_eq!(
        plan(
            every(CatchUp::Skip),
            Some(answered),
            ten_periods_later,
            &ZONE
        ),
        SchedulePlan::Waiting {
            answered_ms: ten_periods_later
        }
    );
}

#[test]
fn a_moment_fires_once_however_late_and_never_again() {
    let moment = Timed::Once { at_ms: 1_000 };
    // Seen after it passed: it fires in this same pass.
    assert_eq!(
        plan(moment, None, 1_500, &ZONE),
        SchedulePlan::Fire {
            due_ms: 1_000,
            answered_ms: 1_000,
            missed: false
        }
    );
    // Answered: it is spent, whatever a later reader's clock says.
    assert_eq!(
        plan(moment, Some(1_000), 10_000, &ZONE),
        SchedulePlan::Waiting { answered_ms: 1_000 }
    );
    // Seen before it comes: nothing fires, and the progress is when it was
    // seen, so a reader that comes back after it passed fires it once.
    assert_eq!(
        plan(moment, Some(500), 900, &ZONE),
        SchedulePlan::Waiting { answered_ms: 500 }
    );
    assert_eq!(
        plan(moment, Some(500), 100_000, &ZONE),
        SchedulePlan::Fire {
            due_ms: 1_000,
            answered_ms: 1_000,
            missed: true
        }
    );
    // Nothing more to wait for once it is answered.
    assert_eq!(moment.waiting_ms(1_000, &ZONE), None);
    assert_eq!(moment.waiting_ms(500, &ZONE), Some(1_000));
    let repeats = every(CatchUp::Skip);
    assert_eq!(repeats.waiting_ms(3_600_001, &ZONE), Some(3_660_000));
}

/// A time of day written with a zone reads back as that zone's and fires there, whatever zone the
/// reader is in; a zone the database does not name is refused with how to write one.
#[test]
fn a_time_of_day_in_a_zone_is_that_zones_wherever_it_is_read() {
    let line = "every day at 08:00 in Europe/Berlin";
    let trigger = crate::trigger_line::trigger_of_line(line).unwrap();
    assert_eq!(crate::trigger_line::trigger_line(&trigger), line);
    let Some(Timed::Repeating { schedule, .. }) = Timed::of(&trigger) else {
        panic!("a schedule: {trigger:?}");
    };
    let at = |moment: &str| moment.parse::<Timestamp>().unwrap();
    // The night Berlin's clocks go forward, 08:00 there moves from 07:00 to 06:00 UTC.
    assert_eq!(
        next_fire_time(schedule, at("2027-03-27T05:00:00Z"), &ZONE),
        Some(at("2027-03-27T07:00:00Z"))
    );
    assert_eq!(
        next_fire_time(schedule, at("2027-03-27T07:00:00Z"), &ZONE),
        Some(at("2027-03-28T06:00:00Z"))
    );
    let refused =
        crate::trigger_line::trigger_of_line("every day at 08:00 in Mars/Olympus").unwrap_err();
    assert!(refused.contains("Europe/Berlin"), "{refused}");
}
