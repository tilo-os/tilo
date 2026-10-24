//! Nothing moves while data arrives: a remote list, media grid and document
//! page, each under a heading and above a line that follows it, laid out
//! frame by frame through the ground, the skeleton, the skeleton naming its
//! source, the answer landing and its cross-fade. Every widget box outside
//! the arriving content (the heading, each reserved box, the line below)
//! stands where it stood on the first frame.

use super::*;
use crate::arriving::{GRACE, NAME_SOURCE_AFTER};
use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::{Column, ListRow, MediaCard, MediaGrid, MediaSource, RowShape, Text};

/// A task's row: its title and the line under it.
const TASK_ROW: RowShape = RowShape::TITLE.with_secondary(1);

/// Where the boxes of the column's parts and of every arriving view stand.
fn boxes(widget: &dyn Widget, found: &mut Vec<(String, Rect)>) {
    if matches!(widget.name(), "ArrivingView" | "Text") {
        found.push((widget.name().to_owned(), widget.get_rect()));
    }
    if widget.name() == "ArrivingView" {
        // What stands inside the box is the content's own business.
        return;
    }
    for child in widget.children() {
        boxes(child, found);
    }
}

/// The page around `body`: a heading above it and a line below it.
fn page(body: Element) -> Column {
    Column::new()
        .with_intrinsic_height()
        .push(Box::new(
            Text::new("Heading").with_role(crate::tokens::TextRole::Title),
        ))
        .push(body)
        .push(Box::new(Text::new("What follows")))
}

/// Lay `view` out at each instant from `t0`, landing `arriving` at `lands`,
/// and fail on any box that moved from where the first frame put it.
fn assert_nothing_moves(name: &str, arriving: &Arriving<()>, view: &mut Column, t0: Instant) {
    let mut frame = ShotFrame::new((720, 900), Theme::light());
    let lands = t0 + NAME_SOURCE_AFTER + Duration::from_millis(100);
    let mut instants = vec![
        t0,
        t0 + GRACE - Duration::from_millis(1),
        t0 + GRACE,
        t0 + Duration::from_millis(500),
        t0 + NAME_SOURCE_AFTER,
    ];
    // The answer lands, then every frame of its cross-fade and after.
    instants.extend((0..40).map(|frame| lands + Duration::from_millis(16 * frame)));
    let mut first: Option<Vec<(String, Rect)>> = None;
    let mut steps = Vec::new();
    for at in instants {
        if at >= lands && arriving.with(Arrival::is_pending) {
            with_monotonic_time(at, || arriving.stand(Arrival::Ready(())));
        }
        with_monotonic_time(at, || {
            frame.sync(view);
            frame.layout(view).expect("layout solves");
            let _ = frame.draw(view);
        });
        let mut found = Vec::new();
        boxes(view, &mut found);
        let mut shown = Vec::new();
        collect_showing(view, &mut shown, at);
        steps.push(shown);
        match &first {
            None => first = Some(found),
            Some(first) => assert_eq!(
                &found,
                first,
                "{name}: a box moved at {:?} after the first frame",
                at - t0
            ),
        }
    }
    assert!(
        steps.iter().any(|step| step.contains(&Step::Named)),
        "{name}: the placeholder named its source ({steps:?})"
    );
    assert!(
        steps
            .last()
            .is_some_and(|step| step.iter().all(|s| *s == Step::Shown)),
        "{name}: the answer shows ({steps:?})"
    );
}

/// The step each arriving view shows at `at`.
fn collect_showing(widget: &dyn Widget, found: &mut Vec<Step>, at: Instant) {
    if let Some(view) = widget
        .as_any()
        .and_then(|any| any.downcast_ref::<ArrivingView>())
    {
        found.push(view.step(at));
        return;
    }
    for child in widget.children() {
        collect_showing(child, found, at);
    }
}

#[test]
fn a_remote_list_moves_nothing_as_its_rows_arrive() {
    let t0 = crate::time::now();
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<()> = Arriving::pending(Source::host("Lounge"));
        let mut rows = Column::new().with_no_padding().with_intrinsic_height();
        for title in ["Water the plants", "Book the train", "Call the bakery"] {
            rows = rows.push(Box::new(
                arriving
                    .view(move |()| -> Element {
                        Box::new(ListRow::new(title, title).secondary("Today"))
                    })
                    .shaped(SkeletonShape::Row(TASK_ROW)),
            ));
        }
        (arriving, page(Box::new(rows)))
    });
    assert_nothing_moves("list", &arriving, &mut view, t0);
}

#[test]
fn a_remote_media_grid_moves_nothing_as_its_cards_arrive() {
    let t0 = crate::time::now();
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<()> = Arriving::pending(Source::host("Lounge"));
        let cards = ["Blue Train", "Kind of Blue", "Time Out", "Moanin'"]
            .into_iter()
            .map(|title| -> Element {
                Box::new(
                    arriving
                        .view(move |()| -> Element {
                            Box::new(
                                MediaCard::new(title, MediaSource::Placeholder)
                                    .title(title)
                                    .meta("Album"),
                            )
                        })
                        .shaped(SkeletonShape::Media(crate::widget::MediaAspect::Square)),
                )
            })
            .collect();
        (arriving, page(Box::new(MediaGrid::of(cards))))
    });
    assert_nothing_moves("grid", &arriving, &mut view, t0);
}

#[test]
fn a_remote_page_moves_nothing_as_its_text_arrives() {
    let t0 = crate::time::now();
    let (arriving, mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<()> = Arriving::pending(Source::host("Lounge"));
        let body = arriving
            .view(|()| -> Element {
                Box::new(Text::new(
                    "It was a bright cold day in April, and the clocks were striking thirteen.",
                ))
            })
            .shaped(SkeletonShape::Page);
        (arriving, page(Box::new(body)))
    });
    assert_nothing_moves("page", &arriving, &mut view, t0);
}

#[test]
fn a_list_names_its_source_once_on_its_first_row() {
    let t0 = crate::time::now();
    let mut frame = ShotFrame::new((720, 600), Theme::light());
    let ((arriving, other), mut view) = with_monotonic_time(t0, || {
        let arriving: Arriving<()> = Arriving::pending(Source::host("Lounge"));
        let other: Arriving<()> = Arriving::pending(Source::peer("Laptop"));
        let mut rows = Column::new().with_no_padding().with_intrinsic_height();
        for read in [&arriving, &arriving, &other, &arriving, &other] {
            rows = rows.push(Box::new(
                read.view(|()| -> Element { Box::new(ListRow::new("row", "row")) })
                    .shaped(SkeletonShape::Row(RowShape::TITLE)),
            ));
        }
        ((arriving, other), rows)
    });
    let named = t0 + NAME_SOURCE_AFTER;
    with_monotonic_time(named, || {
        frame.sync(&mut view);
        frame.layout(&mut view).expect("layout solves");
        let _ = frame.draw(&mut view);
    });
    let names: Vec<bool> = view
        .children()
        .iter()
        .map(|row| {
            let view = row
                .as_any()
                .and_then(|any| any.downcast_ref::<ArrivingView>())
                .expect("an arriving row");
            names_source(&view.naming, &Theme::light())
        })
        .collect();
    assert_eq!(
        names,
        [true, false, true, false, false],
        "each source named once in the region, on its first row"
    );

    // The answers land: the faces they fade out keep the lines that named
    // their sources, rather than falling back to a skeleton bar.
    let lands = named + Duration::from_millis(100);
    with_monotonic_time(lands, || {
        arriving.stand(Arrival::Ready(()));
        other.stand(Arrival::Ready(()));
    });
    let fading = lands + Duration::from_millis(40);
    with_monotonic_time(fading, || {
        frame.sync(&mut view);
        frame.layout(&mut view).expect("layout solves");
        let _ = frame.draw(&mut view);
    });
    let fading_names: Vec<bool> = view
        .children()
        .iter()
        .map(|row| {
            let view = row
                .as_any()
                .and_then(|any| any.downcast_ref::<ArrivingView>())
                .expect("an arriving row");
            assert!(
                view.fade.outgoing(fading).is_some(),
                "the placeholder still fades out"
            );
            view.drawn_naming
        })
        .collect();
    assert_eq!(
        fading_names, names,
        "the outgoing faces name what the placeholders named"
    );
}

#[test]
#[should_panic(expected = "landed in a box reserved as")]
#[cfg(debug_assertions)]
fn a_row_of_another_kind_cannot_land_in_a_rows_box() {
    let mut frame = ShotFrame::new((720, 600), Theme::light());
    let arriving: Arriving<()> = Arriving::ready(());
    // A title-only box, answered with a row that has a secondary line.
    let mut view = arriving
        .view(|()| -> Element { Box::new(ListRow::new("row", "row").secondary("more")) })
        .shaped(SkeletonShape::Row(RowShape::TITLE));
    frame.sync(&mut view);
    frame.layout(&mut view).expect("layout solves");
    let _ = frame.draw(&mut view);
}

/// Whether a failure's face stands inside `widget`: its row, or its
/// banner.
fn shows_failure(widget: &dyn Widget) -> bool {
    matches!(widget.name(), "ListRow" | "Banner")
        || widget.children().into_iter().any(shows_failure)
}

/// Lay a frame of `view` out and draw it at `at`.
fn frame(frame: &mut ShotFrame, view: &mut Column, at: Instant) {
    with_monotonic_time(at, || {
        frame.sync(view);
        frame.layout(view).expect("layout solves");
        let _ = frame.draw(view);
    });
}

/// Which rows of `view` show a failure.
fn failing_rows(view: &Column) -> Vec<bool> {
    view.children().into_iter().map(shows_failure).collect()
}

/// Rows of the reads `order` picks from `reads`, each a task's row.
fn rows_of(reads: &[Arriving<()>], order: &[usize]) -> Column {
    let mut rows = Column::new().with_no_padding().with_intrinsic_height();
    for &read in order {
        rows = rows.push(Box::new(
            reads[read]
                .view(|()| -> Element { Box::new(ListRow::new("row", "row").secondary("Today")) })
                .shaped(SkeletonShape::Row(TASK_ROW)),
        ));
    }
    rows
}

#[test]
fn rows_sharing_a_read_show_its_failure_once_and_rows_of_their_own_each_show_theirs() {
    let t0 = crate::time::now();
    let mut shot = ShotFrame::new((720, 600), Theme::light());
    let (reads, mut view) = with_monotonic_time(t0, || {
        let reads: Vec<Arriving<()>> = vec![
            Arriving::pending(Source::host("Lounge")),
            Arriving::pending(Source::host("Lounge")),
        ];
        let view = rows_of(&reads, &[0, 0, 1, 0, 1]);
        (reads, view)
    });
    frame(&mut shot, &mut view, t0 + NAME_SOURCE_AFTER);
    let fails = t0 + NAME_SOURCE_AFTER + Duration::from_millis(100);
    with_monotonic_time(fails, || {
        for read in &reads {
            read.stand(Arrival::Failed("Lounge did not answer".into()));
        }
    });
    let settled = fails + Duration::from_secs(1);
    frame(&mut shot, &mut view, settled);
    assert_eq!(
        failing_rows(&view),
        [true, false, true, false, false],
        "one failure per read in the region, on its first row"
    );
}

#[test]
fn rows_built_after_their_read_failed_show_its_failure_once() {
    let t0 = crate::time::now();
    let mut shot = ShotFrame::new((720, 600), Theme::light());
    let (_reads, mut view) = with_monotonic_time(t0, || {
        let read: Arriving<()> = Arriving::pending(Source::host("Lounge"));
        read.stand(Arrival::Failed("Lounge did not answer".into()));
        let reads = vec![read];
        let view = rows_of(&reads, &[0, 0, 0]);
        (reads, view)
    });
    frame(&mut shot, &mut view, t0);
    assert_eq!(
        failing_rows(&view),
        [true, false, false],
        "the first frame already shows one failure"
    );
    let boxes_before = {
        let mut found = Vec::new();
        boxes(&view, &mut found);
        found
    };
    frame(&mut shot, &mut view, t0 + Duration::from_millis(16));
    assert_eq!(failing_rows(&view), [true, false, false]);
    let mut boxes_after = Vec::new();
    boxes(&view, &mut boxes_after);
    assert_eq!(boxes_before, boxes_after, "moving the failure moves no box");
}
