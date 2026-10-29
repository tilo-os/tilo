use super::*;
use crate::headless::FrameHarness;
use crate::state::State;
use crate::time::testing::with_monotonic_time;
use crate::widget::{Column, Text};

/// A few lines of content, the first named `inside`.
fn details(key: &str) -> DisclosureBand {
    DisclosureBand::new(
        key,
        Box::new(
            Column::new()
                .with_intrinsic_height()
                .with_no_padding()
                .push(Box::new(Text::new("Inside").with_id("inside")))
                .push(Box::new(Text::new("A second line")))
                .push(Box::new(Text::new("A third line"))),
        ),
    )
}

fn rooted(column: Column) -> FrameHarness {
    let mut d = FrameHarness::for_root(State::new(), column, (640, 400), Theme::dark());
    d.relayout();
    d
}

/// A view that builds its band anew on every change, as views do.
fn rebuild(d: &mut FrameHarness, band: Option<DisclosureBand>) {
    let mut children: Vec<Element> = Vec::new();
    if let Some(band) = band {
        children.push(Box::new(band));
    }
    children.push(Box::new(Text::new("Below").with_id("below")));
    d.root_mut::<Column>().set_children(children);
    d.relayout();
}

/// The first child of the harness's column, as it moves.
fn band(d: &FrameHarness) -> Option<crate::widget::Disclosing> {
    d.root_widget().children()[0].disclosing()
}

/// How far the column's child `index` is drawn below its place.
fn offset_of(d: &FrameHarness, index: usize) -> f64 {
    d.root::<Column>()
        .drawn_offsets()
        .and_then(|offsets| offsets.get(index).copied())
        .unwrap_or(0.0)
}

#[test]
fn a_band_built_after_the_window_has_shown_arrives_over_what_follows() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || rooted(Column::new()));
    with_monotonic_time(start, || rebuild(&mut d, None));
    let empty_y = d.targets()["below"].y0;
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, Some(details("wifi")));
    });
    let Some(crate::widget::Disclosing::Band { extent, drawn }) =
        with_monotonic_time(start, || band(&d))
    else {
        panic!("a band new to the view arrives moving");
    };
    assert!(extent > 20.0, "it is measured at its full height: {extent}");
    assert!(drawn.abs() < 0.5, "and opens from nothing: {drawn}");
    assert!(
        (d.targets()["below"].y0 - empty_y).abs() < 0.5,
        "what follows keeps its laid-out place"
    );
    assert!(with_monotonic_time(start, || offset_of(&d, 1)).abs() < 0.5);

    // What follows moves down with its edge, by transform.
    let midway = start + crate::time::Duration::from_millis(90);
    let moved = with_monotonic_time(midway, || {
        d.relayout();
        offset_of(&d, 1)
    });
    assert!(
        moved > 1.0,
        "what follows is drawn lower as it opens: {moved} of {extent}"
    );
    assert!(with_monotonic_time(midway, || d
        .root_widget()
        .needs_animation_frame()));
}

#[test]
fn an_arrived_band_rests_in_the_layout() {
    let mut d = rooted(Column::new());
    rebuild(&mut d, None);
    let empty_y = d.targets()["below"].y0;
    let frame = crate::motion::pin_frame();
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    rebuild(&mut d, Some(details("wifi")));
    let Some(crate::widget::Disclosing::Band { extent, .. }) = band(&d) else {
        panic!("it arrives moving");
    };
    drop(frame);

    let _settled = crate::motion::testing::settled();
    assert!(band(&d).is_none(), "at rest it is no band on its way");
    assert!(d.root_widget().is_layout_dirty(), "the layout takes it in");
    d.relayout();
    let targets = d.targets();
    assert!(
        (targets["inside"].y0 - empty_y).abs() < 0.5,
        "its content stands where it opened"
    );
    assert!(
        targets["below"].y0 >= empty_y + extent - 0.5,
        "what follows is laid out under it"
    );
    assert!(band(&d).is_none());
    assert!(!d.root_widget().needs_animation_frame());
    assert!(!d.root_widget().is_layout_dirty());
}

#[test]
fn a_band_there_from_the_first_frame_rests_and_built_again_stays() {
    let mut d = rooted(
        Column::new()
            .push(Box::new(details("wifi")))
            .push(Box::new(Text::new("Below").with_id("below"))),
    );
    assert!(band(&d).is_none(), "first paint shows it at rest");
    assert!(!d.root_widget().needs_animation_frame());
    let shown_y = d.targets()["below"].y0;
    let _frame = crate::motion::pin_frame();
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    rebuild(&mut d, Some(details("wifi")));
    assert!(band(&d).is_none(), "built again with its key, it stays");
    assert!(!d.root_widget().needs_animation_frame());
    assert!((d.targets()["below"].y0 - shown_y).abs() < 0.5);
}

#[test]
fn a_band_its_view_stops_building_leaves_and_what_follows_moves_up() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || {
        rooted(
            Column::new()
                .push(Box::new(details("wifi")))
                .push(Box::new(Text::new("Below").with_id("below"))),
        )
    });
    let shown_y = d.targets()["below"].y0;
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, None);
    });
    let empty_y = d.targets()["below"].y0;
    assert!(
        empty_y < shown_y - 20.0,
        "what follows is laid out once where it rests"
    );
    let at_start = with_monotonic_time(start, || offset_of(&d, 0));
    assert!(
        (empty_y + at_start - shown_y).abs() < 0.5,
        "and is drawn where it stood: {at_start}"
    );
    let later = with_monotonic_time(start + crate::time::Duration::from_millis(90), || {
        offset_of(&d, 0)
    });
    assert!(later < at_start - 0.5, "it moves up with the edge: {later}");
    with_monotonic_time(start + crate::time::Duration::from_secs(2), || {
        d.relayout();
        assert!(d.root::<Column>().drawn_offsets().is_none());
        assert!(!d.root_widget().needs_animation_frame());
    });
}

#[test]
fn a_band_with_another_key_arrives_as_the_one_before_leaves() {
    let start = crate::time::now();
    let mut d = with_monotonic_time(start, || {
        rooted(
            Column::new()
                .push(Box::new(details("wifi")))
                .push(Box::new(Text::new("Below").with_id("below"))),
        )
    });
    with_monotonic_time(start, || {
        crate::motion::note_source(crate::tokens::MotionSource::Pointer);
        rebuild(&mut d, Some(details("bluetooth")));
        assert!(
            matches!(band(&d), Some(crate::widget::Disclosing::Band { .. })),
            "another section arrives rather than taking the place of the one before"
        );
        assert!(
            d.root::<Column>().drawn_offsets().is_some(),
            "while the one before leaves"
        );
    });
}

#[test]
fn under_reduced_motion_or_by_key_a_band_lands() {
    let mut d = rooted(Column::new());
    rebuild(&mut d, None);
    let empty_y = d.targets()["below"].y0;
    let _frame = crate::motion::pin_frame();
    d.set_theme(Theme::dark().with_reduce_motion(true));
    d.relayout();
    crate::motion::note_source(crate::tokens::MotionSource::Pointer);
    rebuild(&mut d, Some(details("wifi")));
    assert!(band(&d).is_none(), "nothing below moves");
    assert!(
        d.targets()["below"].y0 > empty_y + 20.0,
        "the height lands at once"
    );
    assert!(
        d.root_widget().needs_animation_frame(),
        "its content fades in"
    );

    let mut d = rooted(Column::new());
    rebuild(&mut d, None);
    crate::motion::note_source(crate::tokens::MotionSource::Keyboard);
    rebuild(&mut d, Some(details("wifi")));
    assert!(band(&d).is_none(), "a key's result lands");
}
