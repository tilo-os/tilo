use super::*;
use tilo_ui_base::{arriving::GRACE, arriving::MIN_SHOWN, arriving::Source};
use crate::time::Duration;

fn ms(base: Instant, ms: u64) -> Instant {
    base + Duration::from_millis(ms)
}

fn pending() -> Arrival<(), &'static str> {
    Arrival::Pending {
        from: Source::Remote,
    }
}

const READY: Arrival<(), &'static str> = Arrival::Ready(());

fn motion() -> Option<EffectiveMotion> {
    Some(Theme::dark().motion())
}

fn step(rows: &ArrivingRows<u32>, key: u32, now: Instant) -> Option<Step> {
    rows.face(
        &key,
        SkeletonShape::Row(tilo_ui_base::widget::RowShape::TITLE),
        now,
    )
    .map(|face| face.step())
}

#[test]
fn arriving_rows_step_through_their_placeholder() {
    let t0 = crate::time::now();

    // A row there from the start shows at once, with no motion, and keeps no timing.
    let mut rows = ArrivingRows::new();
    assert_eq!(rows.observe(&1, &READY, motion(), t0), Showing::Answer);
    assert!(rows.fade(&1, t0).is_none());
    assert!(rows.is_empty(), "a settled row keeps no timing");

    // A pending row shows ground, then a skeleton after the grace period, and redraws
    // only at its steps.
    assert_eq!(
        rows.observe(&1, &pending(), motion(), t0),
        Showing::Placeholder
    );
    assert_eq!(step(&rows, 1, t0), Some(Step::Ground));
    let grace = t0 + GRACE;
    assert_eq!(rows.next_wake_deadline(t0), Some(grace));
    rows.mark_drawn(t0);
    assert!(!rows.needs_redraw(ms(t0, 100)));
    assert!(rows.needs_redraw(t0 + GRACE));
    assert_eq!(step(&rows, 1, grace), Some(Step::Skeleton));
    assert_eq!(rows.next_wake_deadline(grace), None, "then it waits");
    rows.mark_drawn(t0 + GRACE);
    assert!(!rows.needs_redraw(ms(t0, 1_000)));
    assert_eq!(step(&rows, 1, ms(t0, 60_000)), Some(Step::Skeleton));

    // A row that lands before its skeleton showed fades into its slot from the ground.
    let landed = ms(t0, 60);
    assert_eq!(rows.observe(&1, &READY, motion(), landed), Showing::Answer);
    let fade = rows.fade(&1, landed).expect("it fades in");
    assert!(fade.incoming < 1.0);
    assert_eq!(fade.outgoing.map(|(step, _)| step), Some(Step::Ground));
    let settled = ms(t0, 2_000);
    assert!(rows.fade(&1, settled).is_none());
    rows.retain(settled, |_| true);
    assert!(rows.is_empty());

    // A row whose skeleton showed stands its minimum before the row shows.
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

    // A failed row asked again starts its placeholder over.
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
