//! A bot's scheduled fire times are workspace state: the first key holder to
//! answer one writes where it got to, and whichever key holder holds the
//! schedule next carries on from there, catching a missed time up once.

use crate::state::test_desktops::{cert_of, desktops, sync};
use crate::{CLOCK_SKEW_NS, trigger_mark};
use neighbour_wire::placement::{Power, Rank};
use std::time::Duration;
use tilo_automation::{CatchUp, Schedule, TriggerSpec};

const INSTALLATION: &str = "ticker";
const TRIGGER: usize = 0;

fn trigger(seconds: u64) -> TriggerSpec {
    TriggerSpec::Schedule {
        schedule: Schedule::Interval { seconds },
        catch_up: CatchUp::RunOnce,
    }
}

#[test]
fn a_fire_time_one_key_holder_answered_is_where_the_next_one_carries_on() {
    let (mut left, _left_key, mut right, _right_key) = desktops();
    let now = crate::fixture::at();
    let left_cert = cert_of(&left);
    let mark = trigger_mark(&trigger(60));
    assert_eq!(
        right
            .workspace()
            .schedule_answered(INSTALLATION, TRIGGER, &mark),
        None,
        "no key holder has seen it"
    );
    assert!(
        left.workspace_mut()
            .answer_schedule(&left_cert, INSTALLATION, TRIGGER, &mark, 3_660_000, now)
            .unwrap()
    );
    assert_eq!(
        left.workspace()
            .schedule_answered(INSTALLATION, TRIGGER, &mark),
        Some(3_660_000)
    );
    sync(&left, &mut right);
    assert_eq!(
        right
            .workspace()
            .schedule_answered(INSTALLATION, TRIGGER, &mark),
        Some(3_660_000),
        "the answer travels with the workspace, not in one host's file"
    );
    assert_eq!(
        right
            .workspace()
            .schedule_answered(INSTALLATION, TRIGGER, "another trigger"),
        None,
        "a program updated to another trigger starts its own times"
    );
    // An answer never goes backwards, so a late key holder cannot replay a
    // fire time the workspace already answered.
    assert!(
        !left
            .workspace_mut()
            .answer_schedule(&left_cert, INSTALLATION, TRIGGER, &mark, 3_600_000, now)
            .unwrap()
    );
    assert_eq!(
        left.workspace()
            .schedule_answered(INSTALLATION, TRIGGER, &mark),
        Some(3_660_000)
    );
}

#[test]
fn a_key_holder_taking_the_lease_over_answers_where_the_previous_one_stopped() {
    let (mut left, _left_key, mut right, _right_key) = desktops();
    let now = crate::fixture::at();
    let left_cert = cert_of(&left);
    let right_cert = cert_of(&right);
    let mark = trigger_mark(&trigger(60));
    left.workspace_mut()
        .take_lease(
            &left_cert,
            "bot schedule",
            Duration::from_secs(60),
            crate::unranked(),
            now,
        )
        .unwrap()
        .expect("the first key holder holds the schedule");
    left.workspace_mut()
        .answer_schedule(&left_cert, INSTALLATION, TRIGGER, &mark, 3_660_000, now)
        .unwrap();
    let over = now + Duration::from_secs(62) + Duration::from_nanos(CLOCK_SKEW_NS);
    sync(&left, &mut right);
    assert!(
        right
            .workspace_mut()
            .take_lease(
                &right_cert,
                "bot schedule",
                Duration::from_secs(60),
                Rank {
                    power: Power::External,
                    load_permille: 0,
                    latency_ms: 0,
                },
                over,
            )
            .unwrap()
            .is_some(),
        "the desktop that stayed up takes the over lease"
    );
    right
        .workspace_mut()
        .answer_schedule(&right_cert, INSTALLATION, TRIGGER, &mark, 3_720_000, over)
        .unwrap();
    sync(&right, &mut left);
    assert_eq!(
        left.workspace()
            .schedule_answered(INSTALLATION, TRIGGER, &mark),
        Some(3_720_000),
        "the latest answer of every key holder is the progress"
    );
}
