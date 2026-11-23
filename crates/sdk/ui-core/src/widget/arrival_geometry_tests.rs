//! Nothing moves while data arrives: a remote list, media grid and document page, each under a heading
//! and above a line that follows it, laid out frame by frame through the ground, the skeleton, the
//! skeleton held a while, the answer landing and its cross-fade.

use super::*;
use crate::arriving::GRACE;
use crate::launch::ShotFrame;
use crate::time::{Duration, testing::with_monotonic_time};
use crate::widget::{Column, ListRow, MediaCard, MediaGrid, MediaSource, RowShape, Text};

/// A wait long past the skeleton's first frame.
const LATE: Duration = Duration::from_secs(2);

const TASK_ROW: RowShape = RowShape::TITLE.with_secondary(1);

fn boxes(widget: &dyn Widget, found: &mut Vec<(String, Rect)>) {
    if matches!(widget.name(), "ArrivingView" | "Text") {
        found.push((widget.name().to_owned(), widget.get_rect()));
    }
    if widget.name() == "ArrivingView" {
        return;
    }
    for child in widget.children() {
        boxes(child, found);
    }
}

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
    let lands = t0 + LATE + Duration::from_millis(100);
    let mut instants = vec![
        t0,
        t0 + GRACE - Duration::from_millis(1),
        t0 + GRACE,
        t0 + Duration::from_millis(500),
        t0 + LATE,
    ];
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
        steps.iter().any(|step| step.contains(&Step::Skeleton)),
        "{name}: the placeholder showed its skeleton ({steps:?})"
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

/// A remotely-read page whose body `build` makes from the pending read.
fn remote(build: impl FnOnce(&Arriving<()>) -> Element) -> (Arriving<()>, Column, Instant) {
    let t0 = crate::time::now();
    let (arriving, view) = with_monotonic_time(t0, || {
        let arriving: Arriving<()> = Arriving::pending(Source::Remote);
        let body = build(&arriving);
        (arriving, page(body))
    });
    (arriving, view, t0)
}

#[test]
fn a_remote_list_grid_and_page_move_nothing_as_their_content_arrives() {
    let (arriving, mut view, t0) = remote(|arriving| {
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
        Box::new(rows)
    });
    assert_nothing_moves("list", &arriving, &mut view, t0);

    let (arriving, mut view, t0) = remote(|arriving| {
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
        Box::new(MediaGrid::of(cards))
    });
    assert_nothing_moves("grid", &arriving, &mut view, t0);

    let (arriving, mut view, t0) = remote(|arriving| {
        Box::new(
            arriving
                .view(|()| -> Element {
                    Box::new(Text::new(
                        "It was a bright cold day in April, and the clocks were striking thirteen.",
                    ))
                })
                .shaped(SkeletonShape::Page),
        )
    });
    assert_nothing_moves("page", &arriving, &mut view, t0);
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

fn shows_failure(widget: &dyn Widget) -> bool {
    matches!(widget.name(), "ListRow" | "Banner")
        || widget.children().into_iter().any(shows_failure)
}

fn frame(frame: &mut ShotFrame, view: &mut Column, at: Instant) {
    with_monotonic_time(at, || {
        frame.sync(view);
        frame.layout(view).expect("layout solves");
        let _ = frame.draw(view);
    });
}

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
fn a_reads_failure_shows_once_however_its_rows_share_it() {
    let t0 = crate::time::now();
    let mut shot = ShotFrame::new((720, 600), Theme::light());
    let (reads, mut view) = with_monotonic_time(t0, || {
        let reads: Vec<Arriving<()>> = vec![
            Arriving::pending(Source::Remote),
            Arriving::pending(Source::Remote),
        ];
        let view = rows_of(&reads, &[0, 0, 1, 0, 1]);
        (reads, view)
    });
    frame(&mut shot, &mut view, t0 + LATE);
    let fails = t0 + LATE + Duration::from_millis(100);
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

    // Rows built after their read failed show its failure once, and moving the
    // failure moves no box.
    let t0 = crate::time::now();
    let mut shot = ShotFrame::new((720, 600), Theme::light());
    let (_reads, mut view) = with_monotonic_time(t0, || {
        let read: Arriving<()> = Arriving::pending(Source::Remote);
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

/// The widget named `name` inside `widget`, first in tree order.
fn find_named<'a>(widget: &'a dyn Widget, name: &str) -> Option<&'a dyn Widget> {
    if widget.name() == name {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| find_named(child, name))
}

/// How far `pixel` is from every blend of `ground` and `fill`: zero for a
/// pixel that only the two paint, whatever their coverage.
fn off_blend(pixel: [u8; 4], ground: [u8; 4], fill: [u8; 4]) -> f64 {
    let (p, g, f) = (
        pixel.map(f64::from),
        ground.map(f64::from),
        fill.map(f64::from),
    );
    let axis = [f[0] - g[0], f[1] - g[1], f[2] - g[2]];
    let along = axis.iter().map(|a| a * a).sum::<f64>();
    let t = if along > 0.0 {
        ((0..3).map(|i| (p[i] - g[i]) * axis[i]).sum::<f64>() / along).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (0..3)
        .map(|i| (p[i] - (g[i] + t * axis[i])).abs())
        .fold(0.0, f64::max)
}

#[test]
fn a_failed_rows_try_again_shows_nothing_under_its_rounded_corners() {
    for (name, theme) in [("light", Theme::light()), ("dark", Theme::dark())] {
        let t0 = crate::time::now();
        let (width, height) = (720_u32, 560_u32);
        let mut shot = ShotFrame::new((width, height), theme.clone());
        let (_read, mut view) = with_monotonic_time(t0, || {
            let read: Arriving<()> =
                Arriving::pending(Source::Remote).on_retry(|_: &Arriving<()>| {});
            read.stand(Arrival::Failed("Lounge did not answer".into()));
            let view = page(Box::new(rows_of(std::slice::from_ref(&read), &[0])));
            (read, view)
        });
        let scene = with_monotonic_time(t0 + Duration::from_secs(1), || {
            shot.sync(&mut view);
            shot.layout(&mut view).expect("layout solves");
            shot.draw(&mut view)
        });
        let button = find_named(&view, "Button")
            .expect("the failure offers Try again")
            .get_rect();
        let ground = theme.tokens.colors.surfaces.content;
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let mut target =
            tilo_cpu_raster::Target::new(&mut pixels, width, height, width as usize * 4)
                .expect("a raster target");
        tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
            scene.encoding(),
            &mut target,
            None,
            ground,
        );
        let ground = ground.to_rgba8().to_u8_array();
        let pixel = |x: u32, y: u32| -> [u8; 4] {
            let at = ((y * width + x) * 4) as usize;
            [pixels[at], pixels[at + 1], pixels[at + 2], pixels[at + 3]]
        };
        let fill = pixel(button.center().x as u32, button.y0.ceil() as u32 + 1);
        let radius = f64::from(theme.tokens.radii.control);
        // In each corner square only the face and the ground paint: the
        // face's antialiased edge blends the two and nothing else.
        let mut checked = 0;
        let mut stray = Vec::new();
        for y in button.y0.floor() as u32..button.y1.ceil() as u32 {
            for x in button.x0.floor() as u32..button.x1.ceil() as u32 {
                let (cx, cy) = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                let in_corner = (cx < button.x0 + radius || cx > button.x1 - radius)
                    && (cy < button.y0 + radius || cy > button.y1 - radius);
                if !in_corner {
                    continue;
                }
                checked += 1;
                let seen = pixel(x, y);
                if off_blend(seen, ground, fill) > 3.0 {
                    stray.push((x, y, seen));
                }
            }
        }
        assert!(checked > 0, "the button {button:?} has corners to check");
        assert!(
            stray.is_empty(),
            "{name}: paint other than the Try again face and the ground in its corners {button:?} (fill {fill:?}): {stray:?}"
        );
    }
}
