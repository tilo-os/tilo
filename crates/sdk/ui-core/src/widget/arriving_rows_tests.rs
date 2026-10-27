use super::*;
use crate::arriving::{GRACE, MIN_SHOWN, NAME_SOURCE_AFTER, Source};
use crate::time::Duration;

fn ms(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

fn pending() -> Arrival<(), &'static str> {
    Arrival::Pending {
        from: Source::peer("Laptop"),
    }
}

const READY: Arrival<(), &'static str> = Arrival::Ready(());

fn motion() -> Option<EffectiveMotion> {
    Some(Theme::dark().motion())
}

fn step(rows: &ArrivingRows<u32>, key: u32, now: Instant) -> Option<Step> {
    rows.face(
        &key,
        SkeletonShape::Row(crate::widget::RowShape::TITLE),
        now,
    )
    .map(|face| face.step())
}

#[test]
fn a_row_there_from_the_start_shows_at_once_with_no_motion() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    assert_eq!(rows.observe(&1, &READY, motion(), t0), Showing::Answer);
    assert!(rows.fade(&1, t0).is_none());
    assert!(rows.is_empty(), "a settled row keeps no timing");
}

#[test]
fn a_pending_row_shows_ground_then_a_skeleton_then_names_its_source() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    assert_eq!(
        rows.observe(&1, &pending(), motion(), t0),
        Showing::Placeholder
    );
    assert_eq!(step(&rows, 1, t0), Some(Step::Ground));
    let grace = t0 + GRACE;
    assert_eq!(rows.next_wake_deadline(t0), Some(grace));
    assert_eq!(step(&rows, 1, grace), Some(Step::Skeleton));
    let named = t0 + NAME_SOURCE_AFTER;
    assert_eq!(rows.next_wake_deadline(grace), Some(named));
    assert_eq!(step(&rows, 1, named), Some(Step::Named));
    assert_eq!(rows.next_wake_deadline(named), None, "then it waits");
}

#[test]
fn a_row_that_lands_fades_into_its_slot() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    rows.observe(&1, &pending(), motion(), t0);
    let landed = ms(t0, 60);
    assert_eq!(rows.observe(&1, &READY, motion(), landed), Showing::Answer);
    let fade = rows.fade(&1, landed).expect("it fades in");
    assert!(fade.incoming < 1.0);
    assert_eq!(fade.outgoing.map(|(step, _)| step), Some(Step::Ground));
    let settled = ms(t0, 2_000);
    assert!(rows.fade(&1, settled).is_none());
    rows.retain(settled, |_| true);
    assert!(rows.is_empty());
}

#[test]
fn a_row_skeleton_stands_its_minimum_before_the_row_shows() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    rows.observe(&1, &pending(), motion(), t0);
    let landed = ms(t0, 200);
    assert_eq!(
        rows.observe(&1, &READY, motion(), landed),
        Showing::Placeholder,
        "the skeleton showed at 150 ms and stays until 450 ms"
    );
    assert_eq!(step(&rows, 1, landed), Some(Step::Skeleton));
    let shows = t0 + GRACE + MIN_SHOWN;
    assert_eq!(rows.next_wake_deadline(landed), Some(shows));
    assert!(rows.reveal_due(shows));
    assert_eq!(rows.observe(&1, &READY, motion(), shows), Showing::Answer);
    let fade = rows.fade(&1, shows).expect("it fades in");
    assert_eq!(fade.outgoing.map(|(step, _)| step), Some(Step::Skeleton));
}

#[test]
fn rows_redraw_only_at_their_steps() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    rows.observe(&1, &pending(), motion(), t0);
    rows.mark_drawn(t0);
    assert!(!rows.needs_redraw(ms(t0, 100)));
    assert!(rows.needs_redraw(t0 + GRACE));
    rows.mark_drawn(t0 + GRACE);
    assert!(!rows.needs_redraw(ms(t0, 1_000)));
}

#[test]
fn a_failed_row_asked_again_starts_its_placeholder_over() {
    let t0 = crate::time::now();
    let mut rows = ArrivingRows::new();
    rows.observe(&1, &pending(), None, t0);
    let failed: Arrival<(), &str> = Arrival::Failed("offline");
    assert_eq!(
        rows.observe(&1, &failed, None, ms(t0, 50)),
        Showing::Failure
    );
    let again = ms(t0, 1_000);
    assert_eq!(
        rows.observe(&1, &pending(), None, again),
        Showing::Placeholder
    );
    assert_eq!(step(&rows, 1, again), Some(Step::Ground));
    assert_eq!(rows.next_wake_deadline(again), Some(again + GRACE));
}
