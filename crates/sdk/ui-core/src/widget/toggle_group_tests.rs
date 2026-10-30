use super::*;
use crate::presence::PresenceCtx;
use crate::testing::dispatch;
use crate::tokens::TextRole;
use crate::widget::Widget;
use std::cell::RefCell;
use std::rc::Rc;
use taffy::prelude::TaffyMaxContent;
use vello_encoding::DrawTag;

#[test]
fn hosted_mode_swipes_commit_once_and_reject_scroll_cancel_and_multitouch() {
    use winit::event::TouchPhase::{Cancelled, Ended, Moved, Started};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let selected = crate::LocalState::new(0_usize);
    let next = selected.clone();
    let commits = Rc::new(RefCell::new(Vec::new()));
    let receipts = Rc::clone(&commits);
    let mut frame = crate::headless::FrameHarness::for_root(
        crate::State::new(),
        ToggleGroup::new(
            vec![
                Segment::new("Photo"),
                Segment::new("Video"),
                Segment::new("Scan"),
            ],
            0,
        )
        .bind_selection(&selected)
        .on_view("camera-modes", move |index| {
            next.set(index);
            receipts.borrow_mut().push(index);
        }),
        (360, 100),
        Theme::dark(),
    );
    let rect = frame.root::<ToggleGroup>().get_rect();
    let start = rect.center();
    frame.touch(1, Started, start.x, start.y);
    frame.touch(1, Moved, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 0, "motion alone cannot change mode");
    frame.touch(1, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1);
    frame.touch(1, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "a release commits once");
    frame.touch(2, Started, start.x, start.y);
    frame.touch(2, Ended, start.x - 20.0, start.y + 90.0);
    assert_eq!(selected.get(), 1, "vertical scrolling keeps the mode");
    frame.touch(3, Started, start.x, start.y);
    frame.touch(3, Cancelled, start.x - 90.0, start.y);
    frame.touch(3, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "cancelled swipes cannot commit");
    frame.touch(4, Started, start.x, start.y);
    frame.touch(5, Started, start.x, start.y);
    frame.touch(4, Ended, start.x - 90.0, start.y);
    frame.touch(5, Ended, start.x - 90.0, start.y);
    assert_eq!(selected.get(), 1, "multitouch cannot change modes");
    frame.touch(6, Started, start.x, start.y);
    frame.touch(6, Ended, start.x + 90.0, start.y);
    assert_eq!(selected.get(), 0, "right swipe selects the previous mode");
    assert_eq!(
        &*commits.borrow(),
        &[1, 0],
        "only two deliberate swipes invoke the real view operation"
    );
}

#[test]
fn painted_strips_refuse_removed_changed_and_interrupted_touch_owners() {
    use winit::event::TouchPhase::{Cancelled, Ended, Started};
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let rects = [
        Rect::new(0.0, 0.0, 100.0, 48.0),
        Rect::new(100.0, 0.0, 200.0, 48.0),
    ];
    let group = SegmentedTouchGroup::new(42, &rects, 0);
    let mut recognizer = SegmentedTouch::default();
    let event = |id, phase, x, y| winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(x, y),
        force: None,
        id,
    };
    assert_eq!(
        recognizer.handle(&event(1, Started, 180.0, 24.0), 1.0, &[group]),
        SegmentedTouchResult::Captured
    );
    assert_eq!(
        recognizer.handle(&event(1, Ended, 80.0, 24.0), 1.0, &[]),
        SegmentedTouchResult::Captured
    );
    assert!(!recognizer.is_active());
    recognizer.handle(&event(2, Started, 180.0, 24.0), 1.0, &[group]);
    let changed = SegmentedTouchGroup::new(42, &rects, 1);
    assert_eq!(
        recognizer.handle(&event(2, Ended, 80.0, 24.0), 1.0, &[changed]),
        SegmentedTouchResult::Captured
    );
    recognizer.handle(&event(3, Started, 180.0, 24.0), 1.0, &[group]);
    recognizer.handle(&event(4, Started, 180.0, 24.0), 1.0, &[group]);
    recognizer.handle(&event(3, Ended, 80.0, 24.0), 1.0, &[group]);
    recognizer.handle(&event(5, Started, 180.0, 24.0), 1.0, &[group]);
    recognizer.handle(&event(4, Cancelled, 80.0, 24.0), 1.0, &[group]);
    assert_eq!(
        recognizer.handle(&event(5, Ended, 80.0, 24.0), 1.0, &[group]),
        SegmentedTouchResult::Captured
    );
    assert!(!recognizer.is_active());
    recognizer.handle(&event(6, Started, 180.0, 24.0), 1.0, &[group]);
    assert_eq!(
        recognizer.handle(&event(6, Ended, 80.0, 24.0), 1.0, &[group]),
        SegmentedTouchResult::Selected {
            group: 42,
            index: 1
        }
    );
}

/// Fixed rect so tests can simulate clicks without a full taffy pass.
fn group_at(segments: Vec<Segment>, selected: usize, rect: Rect) -> ToggleGroup {
    let mut g = ToggleGroup::new(segments, selected);
    g.rect = rect;
    g.outer = rect;
    // Populate segment_rects manually (mirrors apply_layout).
    let n = g.segments.len();
    if n > 0 {
        let w = rect.width() / f64_of_usize(n);
        let last = n.saturating_sub(1);
        g.segment_rects = (0..n)
            .map(|i| {
                let x0 = rect.x0 + f64_of_usize(i) * w;
                let x1 = if i == last {
                    rect.x1
                } else {
                    rect.x0 + f64_of_usize(i.saturating_add(1)) * w
                };
                Rect::new(x0, rect.y0, x1, rect.y1)
            })
            .collect();
    }
    g
}

use crate::synthetic_input::cursor_moved;

use crate::synthetic_input::mouse_press;

use crate::synthetic_input::named_key;

fn touch(phase: winit::event::TouchPhase, point: Point) -> WindowEvent {
    WindowEvent::Touch(winit::event::Touch {
        device_id: winit::event::DeviceId::dummy(),
        phase,
        location: winit::dpi::PhysicalPosition::new(point.x, point.y),
        force: None,
        id: 7,
    })
}

#[test]
fn a_vertical_rail_stacks_square_segments_bound_to_view_state() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let state = crate::LocalState::new('b');
    let mut rail = ToggleGroup::from_state(
        vec![
            (
                'a',
                Segment::new("Select")
                    .with_icon(Icon::MousePointer2)
                    .with_hint("V"),
            ),
            (
                'b',
                Segment::new("Brush").with_icon(Icon::Brush).with_hint("B"),
            ),
            ('c', Segment::icon(Icon::Type, "Text")),
        ],
        &state,
    )
    .vertical();
    assert_eq!(
        rail.selected(),
        1,
        "the state's value is the chosen segment"
    );
    rail.apply_layout_external(Rect::new(0.0, 0.0, 40.0, 128.0));
    let rects = rail.segment_rects.clone();
    assert_eq!(rects.len(), 3);
    assert!(
        rects
            .iter()
            .all(|rect| (rect.width() - rect.height()).abs() < 1e-9)
    );
    assert!(
        (rects[2].y1 - 128.0).abs() < 1e-9,
        "the gaps share the rest"
    );

    dispatch(&mut rail, &cursor_moved(20.0, rects[2].center().y));
    dispatch(&mut rail, &mouse_press());
    assert_eq!(state.with(|value| *value), 'c');

    // A value set elsewhere (a keyboard shortcut) is shown on sync.
    state.set('a');
    let doc = AutoCommit::new();
    Widget::sync(&mut rail, &doc, &automerge::ROOT);
    assert_eq!(rail.selected(), 0);

    let (_, tip) = rail
        .tooltip_target_at(rects[0].center())
        .expect("an icon segment has a tooltip");
    assert_eq!(tip, "Select  V");
}

#[test]
fn a_press_focuses_without_a_ring_until_the_keys_are_used() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut g = group_at(
        vec![Segment::new("A"), Segment::new("B")],
        0,
        Rect::new(0.0, 0.0, 200.0, 32.0),
    );
    dispatch(&mut g, &cursor_moved(150.0, 16.0));
    dispatch(&mut g, &mouse_press());
    g.set_focus(true);
    assert!(!g.focus_visible(), "a click shows no focus ring");
    dispatch(&mut g, &named_key(NamedKey::ArrowLeft));
    assert!(g.focus_visible(), "the keys bring the ring back");
    g.set_focus(false);
    g.set_focus(true);
    assert!(g.focus_visible(), "keyboard focus shows the ring");
}

#[test]
fn toggle_group_geometry_scales_labels_once_and_is_touch_sized() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    {
        for scale in [1.0, 2.0] {
            let theme = Theme::dark().with_text_scale(scale);
            let mut text = crate::font_catalog::fixture_renderer();
            text.set_text_scale(scale);
            let mut group = group_at(
                vec![Segment::new("Params"), Segment::new("Headers")],
                0,
                Rect::new(0.0, 0.0, 384.0, 52.0),
            );
            let mut scene = Scene::new();
            group.draw(
                &mut scene,
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::default(),
            );
            let runs = &scene.encoding().resources.glyph_runs;
            assert!(!runs.is_empty());
            for run in runs {
                assert!(
                    (run.font_size - theme.tokens.typography.size(TextRole::Label)).abs() < 0.001,
                    "{scale}x text must paint once at resolved size {}, got {}",
                    theme.tokens.typography.size(TextRole::Label),
                    run.font_size
                );
            }
        }
    }

    {
        let harness = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(
                crate::widget::Column::new()
                    .with_no_padding()
                    .push(Box::new(ToggleGroup::new(
                        vec![Segment::new("World"), Segment::new("Timers")],
                        0,
                    ))),
            ),
            (600, 200),
        );
        let rect = harness.inspection().root.children[0].rect;
        assert_eq!(rect[3], ControlSize::Regular.height(&Theme::dark()));
        assert!(
            rect[2] < 300.0,
            "a fitted strip does not stretch to its parent: {rect:?}"
        );
        let mut text = TextRenderer::new();
        let widest = text
            .measure_style(
                "Timers",
                None,
                &Theme::dark().text(TextRole::Label).strong(),
            )
            .0;
        assert!(
            rect[2] / 2.0 >= f64::from(widest) + 2.0 * f64::from(Theme::dark().tokens.spacing.s3),
            "each segment holds the widest label: {rect:?}"
        );
        let harness = crate::headless::FrameHarness::new(
            crate::State::new(),
            Box::new(
                crate::widget::Column::new()
                    .with_no_padding()
                    .push(Box::new(
                        ToggleGroup::new(vec![Segment::new("World"), Segment::new("Timers")], 0)
                            .fill(),
                    )),
            ),
            (600, 200),
        );
        assert_eq!(harness.inspection().root.children[0].rect[2], 600.0);
    }

    {
        let mut g = ToggleGroup::new(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
        );
        // Lay it out with room for its labels, so it shows as a strip.
        g.adopt_theme(&Theme::dark());
        g.measure_naturals(&mut TextRenderer::new());
        g.apply_layout_external(Rect::new(0.0, 0.0, 240.0, g.strip_height()));
        assert!(!g.is_folded(), "three short labels fit in 240 px");

        assert_eq!(
            g.separator_count(),
            1,
            "a separator stands only between two unchosen neighbours"
        );

        // `>=` since fills/border/text also contribute `DrawTag::COLOR`
        // entries; the floor proves the separator strokes were emitted.
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let presence = PresenceCtx::default();
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);

        let colour_tags = scene
            .encoding()
            .draw_tags
            .iter()
            .filter(|t| **t == DrawTag::COLOR)
            .count();

        assert!(
            colour_tags >= 2,
            "3-segment ToggleGroup must emit >= 2 colour draw-tags for separator lines (got {colour_tags})"
        );
    }

    {
        let theme = Theme::dark();
        let mut tr = TextRenderer::new();
        let segs = [
            Rect::new(0.0, 0.0, 60.0, 28.0),
            Rect::new(60.0, 0.0, 120.0, 28.0),
            Rect::new(120.0, 0.0, 180.0, 28.0),
        ];
        let labels = ["A", "B", "C"];
        for style in [
            SegmentedStyle::Segmented,
            SegmentedStyle::Tabs,
            SegmentedStyle::Quiet,
        ] {
            let mut scene = Scene::new();
            ToggleGroup::paint(
                &mut scene,
                Affine::IDENTITY,
                &mut tr,
                &theme,
                &SegmentedFace::new(&segs, &labels, 1).style(style),
            );
            let colour_tags = scene
                .encoding()
                .draw_tags
                .iter()
                .filter(|t| **t == DrawTag::COLOR)
                .count();
            assert!(colour_tags > 0, "{style:?} paint must emit draw commands");
        }
    }

    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut group = group_at(
            vec![
                Segment::new("Monday").short("Mon"),
                Segment::new("Tuesday").short("Tue"),
            ],
            0,
            Rect::new(0.0, 0.0, 90.0, 32.0),
        );
        group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
        group.measure_naturals(&mut text);
        group.set_rect_and_partition(Rect::new(0.0, 0.0, 90.0, 32.0));
        assert_eq!(group.shown_labels(), ["Mon", "Tue"]);
        group.set_rect_and_partition(Rect::new(0.0, 0.0, 400.0, 32.0));
        assert_eq!(group.shown_labels(), ["Monday", "Tuesday"]);
    }

    // Case `segments_size_to_their_labels_before_any_label_shortens`.
    // "Day / Week / Year" in a strip too narrow for three equal "Week"
    // segments but wide enough for the three labels: no segment elides,
    // the wider label gets the wider segment.
    {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let mut group = group_at(
            vec![
                Segment::new("Day"),
                Segment::new("Week"),
                Segment::new("Year"),
            ],
            1,
            Rect::new(0.0, 0.0, 400.0, 32.0),
        );
        group.metrics = StripMetrics::of(ControlSize::Regular, &theme);
        group.measure_naturals(&mut text);
        let naturals: Vec<f64> = group.naturals.iter().map(|(full, _)| *full).collect();
        let widest = naturals.iter().copied().fold(0.0, f64::max);
        let sum: f64 = naturals.iter().sum();
        assert!(
            sum < widest * 3.0,
            "the labels differ in width: {naturals:?}"
        );

        // Room for three equal widest segments: equal widths.
        group.set_rect_and_partition(Rect::new(0.0, 0.0, (widest * 3.0).ceil(), 32.0));
        let widths: Vec<f64> = group.segment_rects.iter().map(Rect::width).collect();
        assert!(
            widths.iter().all(|w| (w - widths[0]).abs() < 1e-6),
            "{widths:?}"
        );

        // Less than that but room for every label: sized to the labels.
        let tight = (sum + widest * 3.0) / 2.0;
        group.set_rect_and_partition(Rect::new(0.0, 0.0, tight, 32.0));
        for (rect, natural) in group.segment_rects.iter().zip(&naturals) {
            assert!(rect.width() + 1e-6 >= *natural, "{rect:?} holds {natural}");
        }
        assert_eq!(group.shown_labels(), ["Day", "Week", "Year"]);
        assert!(
            (group.segment_rects.last().unwrap().x1 - tight).abs() < 1e-6,
            "the strip is filled exactly"
        );
    }
}

#[test]
fn a_disabled_segment_is_never_chosen_and_the_keys_step_over_it() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    let mut g = group_at(
        vec![
            Segment::new("Whole"),
            Segment::new("Selection").with_enabled(false),
            Segment::new("View"),
        ],
        0,
        Rect::new(0.0, 0.0, 300.0, 36.0),
    );
    dispatch(&mut g, &cursor_moved(150.0, 18.0));
    dispatch(&mut g, &mouse_press());
    assert_eq!(g.selected(), 0, "a click on it chooses nothing");
    g.set_focus(true);
    assert!(dispatch(&mut g, &named_key(NamedKey::ArrowRight)));
    assert_eq!(g.selected(), 2, "the arrow steps over it");
    assert!(dispatch(&mut g, &named_key(NamedKey::ArrowLeft)));
    assert_eq!(g.selected(), 0);
    let node = g.accessibility().expect("a group node");
    assert!(
        node.children
            .get(1)
            .is_some_and(|segment| segment.state.disabled)
    );
}

#[test]
fn toggle_group_selection_follows_pointer_touch_and_keyboard() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    {
        let g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
        assert_eq!(g.selected(), 0);
    }

    {
        let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
        let captured_clone = captured.clone();
        let mut g = group_at(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        )
        .on_change(move |i| {
            *captured_clone.borrow_mut() = Some(i);
        });

        dispatch(&mut g, &cursor_moved(250.0, 18.0));
        dispatch(&mut g, &mouse_press());

        assert_eq!(g.selected(), 2, "click in segment 2 must select index 2");
        assert_eq!(
            *captured.borrow(),
            Some(2),
            "on_change must fire with the new index"
        );
    }

    {
        let changes = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&changes);
        let mut group = ToggleGroup::new(
            vec![
                Segment::new("One"),
                Segment::new("Two"),
                Segment::new("Three"),
            ],
            0,
        )
        .on_change(move |index| captured.borrow_mut().push(index));
        group.set_focus(true);

        assert!(dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
        assert_eq!(group.selected(), 1);
        assert!(dispatch(&mut group, &named_key(NamedKey::End)));
        assert_eq!(group.selected(), 2);
        assert!(dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
        assert_eq!(group.selected(), 0, "arrow navigation wraps");
        assert!(dispatch(&mut group, &named_key(NamedKey::Enter)));
        assert_eq!(&*changes.borrow(), &[1, 2, 0]);

        group.set_focus(false);
        assert!(!dispatch(&mut group, &named_key(NamedKey::ArrowRight)));
        assert_eq!(
            group.selected(),
            0,
            "unfocused controls do not consume keys"
        );
    }

    {
        let mut group = group_at(
            vec![
                Segment::new("World"),
                Segment::new("Alarms"),
                Segment::new("Timers"),
            ],
            0,
            Rect::new(0.0, 0.0, 300.0, 44.0),
        );
        dispatch(
            &mut group,
            &touch(winit::event::TouchPhase::Started, Point::new(250.0, 22.0)),
        );
        assert_eq!(group.selected(), 0, "touch down does not switch a tab");
        dispatch(
            &mut group,
            &touch(winit::event::TouchPhase::Ended, Point::new(250.0, 22.0)),
        );
        assert_eq!(group.selected(), 2);

        group.set_focus(true);
        dispatch(&mut group, &named_key(NamedKey::ArrowRight));
        assert_eq!(group.selected(), 0, "right arrow wraps to the first tab");
        dispatch(&mut group, &named_key(NamedKey::End));
        assert_eq!(group.selected(), 2);
    }

    {
        let mut group = group_at(
            vec![Segment::new("Light"), Segment::new("Dark")],
            0,
            Rect::new(0.0, 0.0, 200.0, 44.0),
        )
        .on_view("appearance", |_| {});
        crate::widget::controls::refresh_control_action_availability(
            &mut group,
            &std::collections::HashSet::new(),
        );
        group.set_focus(true);
        assert!(!Widget::is_enabled(&group));

        dispatch(&mut group, &cursor_moved(150.0, 22.0));
        assert!(!dispatch(&mut group, &mouse_press()));
        assert!(!dispatch(
            &mut group,
            &touch(winit::event::TouchPhase::Started, Point::new(150.0, 22.0)),
        ));
        assert!(!dispatch(&mut group, &named_key(NamedKey::ArrowRight),));
        assert_eq!(group.selected(), 0);
    }
}

// `TILO_SEGMENT_FRAMES=<dir>` writes each frame of the slide as a PNG strip to
// look at.
#[test]
fn a_click_slides_the_chosen_pill_from_the_old_segment_to_the_new_one() {
    use crate::time::testing::with_monotonic_time;
    let strip = std::env::var_os("TILO_SEGMENT_FRAMES").map(std::path::PathBuf::from);
    for (name, theme) in [("dark", Theme::dark()), ("light", Theme::light())] {
        let sizes = ["Small", "Default", "Large", "Larger"];
        let chosen = crate::LocalState::new("Default".to_owned());
        let t0 = crate::time::now();
        let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
        let mut frame = with_monotonic_time(t0, || {
            crate::headless::FrameHarness::for_root(
                crate::State::new(),
                ToggleGroup::from_state(
                    sizes
                        .iter()
                        .map(|size| ((*size).to_owned(), Segment::new(*size)))
                        .collect(),
                    &chosen,
                ),
                (520, 72),
                theme.clone(),
            )
        });
        let (from, to) = {
            let group = frame.root::<ToggleGroup>();
            (group.segment_rects[1], group.segment_rects[2])
        };
        assert!(to.x0 > from.x0, "{name}: Large sits right of Default");
        // Rasterised on the CPU, so the strip needs no GPU.
        let write = |frame: &crate::headless::FrameHarness, ms: u64| {
            let Some(dir) = &strip else {
                return;
            };
            let (width, height) = (520_u32, 72_u32);
            let mut pixels = vec![0u8; (width * height * 4) as usize];
            let mut target =
                tilo_cpu_raster::Target::new(&mut pixels, width, height, width as usize * 4)
                    .expect("a raster target");
            tilo_cpu_raster::CpuRasterizer::new(tilo_cpu_raster::Options::default()).render(
                frame.scene().encoding(),
                &mut target,
                None,
                theme.tokens.colors.surfaces.content,
            );
            std::fs::create_dir_all(dir).expect("the strip's directory");
            crate::png::write_rgba8(
                dir.join(format!("{name}-{ms:04}ms.png")),
                width,
                height,
                &pixels,
            )
            .expect("a frame of the strip");
        };
        write(&frame, 0);
        with_monotonic_time(at(16), || frame.click_at(to.center().x, to.center().y));
        assert_eq!(chosen.get(), "Large", "{name}: the click chooses Large");
        write(&frame, 16);
        let mut between = 0;
        let mut last_x = from.x0;
        for ms in (32..=1_000).step_by(16) {
            with_monotonic_time(at(ms), || {
                let _ = frame.poll_frame();
            });
            write(&frame, ms);
            let group = frame.root::<ToggleGroup>();
            assert_eq!(group.selected(), 2, "{name}: Large stays chosen");
            let Some(pill) = group.last_thumb else {
                continue;
            };
            // On its way: strictly between the segments, moving one way.
            assert!(
                pill.x0 > from.x0 && pill.x0 < to.x0,
                "{name} at {ms} ms: the pill {pill:?} is between {from:?} and {to:?}"
            );
            let (narrow, wide) = (from.width().min(to.width()), from.width().max(to.width()));
            assert!(
                pill.width() >= narrow - 0.5 && pill.width() <= wide + 0.5,
                "{name} at {ms} ms: the pill's width {} is between the segments' {narrow}..{wide}",
                pill.width()
            );
            if (wide - narrow).abs() > 1.0 {
                assert!(
                    pill.width() > narrow && pill.width() < wide,
                    "{name} at {ms} ms: the width moves too"
                );
            }
            assert!(
                pill.x0 >= last_x,
                "{name} at {ms} ms: the pill never turns back"
            );
            last_x = pill.x0;
            between += 1;
        }
        assert!(
            between >= 3,
            "{name}: the pill slides over several frames, not a jump ({between} between)"
        );
        let group = frame.root::<ToggleGroup>();
        assert!(
            !with_monotonic_time(at(1_000), || group.needs_animation_frame()),
            "{name}: the slide settles"
        );
        assert_eq!(
            group.last_thumb, None,
            "{name}: the pill rests on its segment"
        );
        assert_eq!(
            group.thumb.get(at(1_000)),
            group.segment_rects[2],
            "{name}: at rest the pill is exactly Large"
        );
    }
}

#[test]
fn hovering_a_segment_of_a_single_choice_draws_no_second_pill() {
    let segments = [
        Rect::new(0.0, 0.0, 100.0, 32.0),
        Rect::new(100.0, 0.0, 200.0, 32.0),
        Rect::new(200.0, 0.0, 300.0, 32.0),
    ];
    let items = [
        Segment::new("Small"),
        Segment::new("Default"),
        Segment::new("Large"),
    ];
    for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
        let mut text = TextRenderer::new();
        let mut drawn = |hovered: Option<usize>, chosen: Option<&[bool]>| {
            let mut face = SegmentedFace::of_segments(&segments, &items, 1).hovered(hovered);
            if let Some(chosen) = chosen {
                face = face.chosen(chosen);
            }
            let mut scene = Scene::new();
            ToggleGroup::paint(&mut scene, Affine::IDENTITY, &mut text, &theme, &face);
            scene.encoding().draw_data.clone()
        };
        assert_eq!(
            drawn(Some(2), None),
            drawn(None, None),
            "{}: the pointer over Large draws nothing beside the chosen Default",
            theme.name
        );
        let chosen = [false, true, false];
        assert_ne!(
            drawn(Some(2), Some(&chosen)),
            drawn(None, Some(&chosen)),
            "{}: a multi-select strip lifts the segment under the pointer",
            theme.name
        );
    }
}

/// Choosing another segment by pointer slides the thumb there from the
/// segment it left; a key's choice puts it there at once.
#[test]
fn the_thumb_slides_to_a_clicked_segment_and_lands_on_a_key() {
    use crate::time::testing::with_monotonic_time;
    use crate::tokens::MotionSource;
    let theme = Theme::dark();
    let mut g = ToggleGroup::new(
        vec![
            Segment::new("Day"),
            Segment::new("Week"),
            Segment::new("Month"),
        ],
        0,
    )
    // The strip fits its labels through `measure`, which a plain taffy
    // pass never calls: let it take the row's width instead.
    .fill();
    let mut taffy = TaffyTree::<()>::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let node = g.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy.parent_node(
        Style {
            size: Size::from_lengths(300.0, 40.0),
            ..Style::default()
        },
        &[node],
    );
    taffy
        .compute_layout(root, taffy::Size::MAX_CONTENT)
        .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);
    let mut tr = TextRenderer::new();
    let presence = PresenceCtx::default();
    let mut paint = |g: &mut ToggleGroup| {
        let mut scene = Scene::new();
        g.draw(&mut scene, Affine::IDENTITY, &mut tr, &theme, &presence);
    };
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    with_monotonic_time(t0, || paint(&mut g));
    assert!(!g.needs_animation_frame(), "the first paint rests");
    let (first, last) = (g.segment_rects[0], g.segment_rects[2]);
    with_monotonic_time(t0, || {
        crate::motion::note_source(MotionSource::Pointer);
        g.set_selected(2);
        paint(&mut g);
        assert!(
            g.needs_animation_frame(),
            "the thumb slides: segments {:?}, thumb target {:?}, thumb for {}, selected {}, chosen {:?}, settles {:?}, source {:?}, reduced {}",
            g.segment_rects,
            g.thumb.target(),
            g.thumb_for,
            g.selected,
            g.chosen,
            g.thumb.settles_at(),
            crate::motion::input_source(),
            theme.motion().reduced()
        );
    });
    with_monotonic_time(at(60), || {
        paint(&mut g);
        let drawn = g.last_thumb.expect("on its way");
        assert!(
            drawn.x0 > first.x0 && drawn.x0 < last.x0,
            "between the segments: {drawn:?}, first {first:?}, last {last:?}, value now {:?}, target {:?}",
            g.thumb.get(crate::time::now()),
            g.thumb.target()
        );
    });
    with_monotonic_time(at(600), || {
        paint(&mut g);
        assert!(!g.needs_animation_frame());
        assert_eq!(g.last_thumb, None, "landed on its segment");
        crate::motion::note_source(MotionSource::Keyboard);
        g.set_selected(0);
        paint(&mut g);
        assert!(!g.needs_animation_frame(), "a key's choice lands at once");
    });
}

/// A focus change swaps the outer border colour, so the encoded stream
/// differs between focused and unfocused renders. The strip has room for
/// both labels, so it shows as a strip.
fn group_draw_data(focused: bool, theme: &Theme) -> Vec<u32> {
    let mut g = ToggleGroup::new(vec![Segment::new("a"), Segment::new("b")], 0);
    let mut tr = TextRenderer::new();
    g.adopt_theme(theme);
    g.measure_naturals(&mut tr);
    g.apply_layout_external(Rect::new(0.0, 0.0, 240.0, g.strip_height()));
    assert!(!g.is_folded(), "two short labels fit in 240 px");
    g.set_focus(focused);

    let presence = PresenceCtx::default();
    let mut scene = Scene::new();
    g.draw(&mut scene, Affine::IDENTITY, &mut tr, theme, &presence);
    scene.encoding().draw_data.clone()
}

#[test]
fn segment_colours_follow_selection_focus_and_style() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    // Case `focused_toggle_group_uses_the_focus_colour`.
    // Uses high contrast (yellow focus vs white border) so the two colours
    // are unambiguously different.
    {
        let theme = Theme::high_contrast();
        let unfocused = group_draw_data(false, &theme);
        let focused = group_draw_data(true, &theme);
        assert_ne!(
            unfocused, focused,
            "focusing the group must change the border colour in the encoded scene",
        );
    }

    // Case `the_chosen_segment_is_a_lift_with_readable_ink`: the thumb is
    // its style's selector lift (on a light raised track the one white
    // object in the well), tinted at most by the accent's faint hue.
    {
        use crate::a11y::contrast_ratio;
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let colors = &theme.tokens.colors;
            for (style, ground) in [
                (SegmentedStyle::Segmented, colors.surfaces.raised),
                (SegmentedStyle::Tabs, colors.surfaces.content),
                (SegmentedStyle::Quiet, colors.surfaces.content),
            ] {
                let (fill, ink) = segment_colors(&theme, style, true, false);
                let fill = fill.expect("the chosen segment is lifted");
                assert_eq!(
                    fill,
                    segment_tier(&theme, style, SelectionState::active(true)).fill,
                    "{} {style:?}: the thumb is its style's selector lift",
                    theme.name
                );
                let chroma = crate::color::Oklch::from_color(fill).c;
                assert!(
                    chroma <= crate::tokens::palette::TINTED_LIFT_CHROMA + 0.01,
                    "{} {style:?}: the chosen segment carries more than a faint tint ({chroma})",
                    theme.name
                );
                assert!(contrast_ratio(ink, fill) >= ON_FILL_MIN_CONTRAST);
                let (idle, idle_ink) = segment_colors(&theme, style, false, false);
                assert_eq!(idle, None, "{}: an idle segment paints nothing", theme.name);
                assert!(
                    contrast_ratio(idle_ink, ground) < contrast_ratio(colors.text_primary, ground),
                    "{} {style:?}: an idle label is quieter than the chosen one",
                    theme.name
                );
                let (hover, hover_ink) = segment_colors(&theme, style, false, true);
                assert!(hover.is_some(), "a hovered segment lifts");
                assert_eq!(hover_ink, idle_ink, "the label holds its colour on hover");
            }
            // A page tab is a selector like a window's tab: the same
            // tier, so the two read as one family.
            let (fill, _) = segment_colors(&theme, SegmentedStyle::Tabs, true, false);
            assert_eq!(
                fill,
                Some(
                    crate::chrome::selector_tier(
                        &theme,
                        SelectionState::active(true),
                        colors.surfaces.content
                    )
                    .fill
                ),
                "{}: a chosen tab is the selector lift of the window ground",
                theme.name
            );
        }
    }
}

#[test]
fn accessibility_and_actions_expose_and_drive_the_selected_segment() {
    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    {
        use crate::a11y::Role;
        let g = ToggleGroup::new(
            vec![
                Segment::new("Light"),
                Segment::new("Dark"),
                Segment::new("Auto"),
            ],
            1,
        );
        let node = Widget::accessibility(&g).expect("toggle group is not presentational");
        assert_eq!(node.role, Role::Group);
        assert_eq!(node.name, None, "no intrinsic label on the group");
        assert_eq!(
            node.value.as_deref(),
            Some("Dark"),
            "value is the selected segment's label",
        );
        assert!(node.state.focusable);
        assert!(!node.state.disabled);
    }

    {
        // Nothing reads `captured` back in this test (only the accessibility
        // value is asserted), so the closure can just own it — no need for
        // the clone-and-keep-a-handle dance the other click test uses.
        let captured: Rc<RefCell<Option<usize>>> = Rc::new(RefCell::new(None));
        let mut g = group_at(
            vec![Segment::new("a"), Segment::new("b"), Segment::new("c")],
            0,
            Rect::new(0.0, 0.0, 300.0, 36.0),
        )
        .on_change(move |i| *captured.borrow_mut() = Some(i));

        assert_eq!(
            Widget::accessibility(&g).unwrap().value.as_deref(),
            Some("a")
        );
        let segments = Widget::accessibility(&g).unwrap().children;
        assert_eq!(
            segments.get(2).and_then(|segment| segment.bounds),
            g.segment_rects.get(2).map(|rect| (*rect).into()),
            "each segment is placed where it is drawn"
        );

        // Click into segment 2 → value follows the new selection.
        dispatch(&mut g, &cursor_moved(250.0, 18.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 2);
        assert_eq!(
            Widget::accessibility(&g).unwrap().value.as_deref(),
            Some("c")
        );
    }

    {
        let changed = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&changed);
        let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
            .on_change(move |index| captured.borrow_mut().push(index));

        assert!(Widget::perform_semantic_action(
            &mut group,
            "segment:1",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(group.selected(), 1);
        assert_eq!(&*changed.borrow(), &[1]);
    }

    {
        let changed = Rc::new(RefCell::new(Vec::new()));
        let captured = Rc::clone(&changed);
        let mut group = ToggleGroup::new(vec![Segment::new("Light"), Segment::new("Dark")], 0)
            .on_view("appearance", move |index| captured.borrow_mut().push(index));

        let resources = crate::resources::resources(&group, &AutoCommit::new());
        assert_eq!(resources.len(), 1);
        let resource = &resources[0];
        assert_eq!(resource.id.as_str(), "tilo.control:appearance");
        assert_eq!(resource.actions.len(), 1);
        let spec = &resource.actions[0];
        assert_eq!(spec.kind, crate::tilo_actions::OperationKind::View);
        assert_eq!(spec.params()[0].name, "index");

        assert!(Widget::perform_semantic_action(
            &mut group,
            "appearance:segment:1",
            &crate::a11y::Action::Activate,
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        ));
        assert_eq!(
            group.selected(),
            0,
            "UI intent does not bypass the action host"
        );
        assert!(changed.borrow().is_empty());

        let invocation = crate::widget::controls::take_control_invocation(&mut group).unwrap();
        assert_eq!(
            invocation.address,
            crate::tilo_actions::ActionAddress::new("tilo.control:appearance", "select")
        );
        assert_eq!(invocation.arguments["index"], 1);
        let encoded = spec.build_json(&invocation.arguments).unwrap();
        assert_eq!(encoded.type_id, "tilo.ui.segmented.select");
        let output = crate::widget::start_operation(
            &mut group,
            &crate::widget::ResourceOperation::local(
                crate::tilo_actions::InvocationId::new(),
                crate::tilo_actions::OperationKind::View,
                &encoded.type_id,
                &encoded.payload,
            ),
            &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
            &automerge::ROOT,
        )
        .map(|started| started.and_then(crate::widget::ResourceOperationStart::completed))
        .unwrap();
        assert!(output.is_some());
        assert_eq!(group.selected(), 1);
        assert_eq!(&*changed.borrow(), &[1]);
    }
}

#[test]
fn strips_choose_nothing_many_or_icons_and_tabs_start_flush() {
    fn nothing_chosen_until_the_person_picks() {
        let mut g = group_at(
            vec![Segment::new("Low"), Segment::new("High")],
            NONE_CHOSEN,
            Rect::new(0.0, 0.0, 200.0, 32.0),
        );
        assert_eq!(g.selection(), None);
        assert_eq!(g.separator_count(), 1);
        let node = g.accessibility().expect("semantics");
        assert_eq!(node.value, None);
        g.set_focus(true);
        dispatch(&mut g, &named_key(NamedKey::ArrowRight));
        assert_eq!(g.selection(), Some(0), "the first arrow chooses the first");
        g.set_selection(None);
        assert_eq!(g.selected(), NONE_CHOSEN);
    }

    fn a_bound_selection_follows_its_state_both_ways() {
        let state = crate::LocalState::new(1usize);
        let mut g =
            ToggleGroup::new(vec![Segment::new("A"), Segment::new("B")], 0).bind_selection(&state);
        assert_eq!(g.selected(), 1);
        g.rect = Rect::new(0.0, 0.0, 200.0, 32.0);
        g.segment_rects = partition(g.rect, 2);
        dispatch(&mut g, &cursor_moved(20.0, 16.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(state.get(), 0);
        state.set(1);
        g.sync(&AutoCommit::new(), &automerge::ROOT);
        assert_eq!(g.selected(), 1);
    }

    fn many_toggle_on_their_own_and_wrap() {
        let days = ["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];
        let state = crate::LocalState::new(std::collections::BTreeSet::from([0_usize]));
        let mut g = ToggleGroup::multiple_from_state(
            days.iter()
                .enumerate()
                .map(|(index, d)| (index, Segment::new(*d)))
                .collect(),
            &state,
        );
        let mut text = TextRenderer::new();
        g.measure_naturals(&mut text);
        g.apply_layout_external(Rect::new(0.0, 0.0, 160.0, 200.0));
        assert!(g.rows > 1, "seven toggles wrap in 160 px: {} rows", g.rows);
        let second = g.segment_rects[1];
        dispatch(&mut g, &cursor_moved(second.center().x, second.center().y));
        dispatch(&mut g, &mouse_press());
        assert_eq!(state.get(), std::collections::BTreeSet::from([0, 1]));
        let node = g.accessibility().expect("semantics");
        assert_eq!(node.children[1].state.checked, Some(true));
        state.set(std::collections::BTreeSet::from([6]));
        g.sync(&AutoCommit::new(), &automerge::ROOT);
        assert_eq!(
            g.chosen().map(<[bool]>::to_vec),
            Some(vec![false, false, false, false, false, false, true])
        );
    }

    fn icon_segments_are_named_and_square() {
        let segments = vec![
            Segment::icon(crate::icons::Icon::List, "List"),
            Segment::icon(crate::icons::Icon::LayoutGrid, "Grid"),
        ];
        let mut g = ToggleGroup::new(segments, 0);
        let mut text = TextRenderer::new();
        g.measure_naturals(&mut text);
        assert!((g.naturals[0].0 - g.metrics.height.ceil()).abs() < 1e-9);
        let node = g.accessibility().expect("semantics");
        assert_eq!(node.children[1].name.as_deref(), Some("Grid"));
    }

    fn a_disabled_strip_takes_no_input() {
        let mut g = group_at(
            vec![Segment::new("A"), Segment::new("B")],
            0,
            Rect::new(0.0, 0.0, 200.0, 32.0),
        )
        .with_enabled(false);
        dispatch(&mut g, &cursor_moved(150.0, 16.0));
        dispatch(&mut g, &mouse_press());
        assert_eq!(g.selected(), 0);
        assert!(!g.is_focusable());
    }

    fn tabs_start_on_the_leading_edge_and_scroll_when_long() {
        let theme = Theme::dark();
        let mut text = TextRenderer::new();
        let labels: Vec<Segment> = ["Overview", "Activity", "Settings", "Members", "Billing"]
            .iter()
            .map(|l| Segment::new(*l))
            .collect();
        let rects = SegmentedFace::partition_segments(
            Rect::new(10.0, 0.0, 600.0, 32.0),
            &labels,
            SegmentedStyle::Tabs,
            ControlSize::Regular,
            &mut text,
            &theme,
        );
        assert!((rects[0].x0 - 10.0).abs() < 1e-9);
        let metrics = StripMetrics::of(ControlSize::Regular, &theme);
        let (x0, _) = tab_label_span(&rects[0], "Overview", &metrics, &mut text);
        assert!(
            (x0 - 10.0 - metrics.pad_x).abs() < 1e-9,
            "the first label sits its padding inside the first tab"
        );

        let mut g = ToggleGroup::new(labels, 4).with_style(SegmentedStyle::Tabs);
        g.metrics = metrics.clone();
        g.measure_naturals(&mut text);
        g.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 32.0));
        assert!(g.overflows());
        g.reveal(4);
        let last = g.segment_rects[4];
        assert!(
            last.x1 <= 200.0 + 1e-6 && last.x0 >= 0.0,
            "the chosen tab scrolls into view: {last:?}"
        );

        // Tabs with short labels take them before they scroll.
        let named = |short: bool| -> Vec<Segment> {
            [("Privacy & Security", "Privacy"), ("Apps & Files", "Apps")]
                .iter()
                .map(|(full, brief)| {
                    let tab = Segment::new(*full);
                    if short { tab.short(*brief) } else { tab }
                })
                .collect()
        };
        let mut g = ToggleGroup::new(named(true), 0).with_style(SegmentedStyle::Tabs);
        g.metrics = metrics.clone();
        g.measure_naturals(&mut text);
        g.apply_layout_external(Rect::new(0.0, 0.0, 600.0, 32.0));
        assert_eq!(g.shown_labels(), ["Privacy & Security", "Apps & Files"]);
        let full_width = g.segment_rects[1].x1;
        let room = full_width - 1.0;
        g.apply_layout_external(Rect::new(0.0, 0.0, room, 32.0));
        assert_eq!(g.shown_labels(), ["Privacy", "Apps"]);
        assert!(!g.overflows(), "the short labels fit");
        assert!(g.segment_rects[1].x1 <= room + 1e-6);
        let mut plain = ToggleGroup::new(named(false), 0).with_style(SegmentedStyle::Tabs);
        plain.metrics = metrics;
        plain.measure_naturals(&mut text);
        plain.apply_layout_external(Rect::new(0.0, 0.0, room, 32.0));
        assert!(plain.overflows(), "without short labels the strip scrolls");
    }

    fn a_caption_names_the_strip_and_sits_above_it() {
        let mut g =
            ToggleGroup::new(vec![Segment::new("A"), Segment::new("B")], 0).caption("Repeat");
        g.adopt_theme(&Theme::dark());
        g.apply_layout_external(Rect::new(0.0, 0.0, 200.0, 60.0));
        assert!(g.rect.y0 > 0.0);
        assert_eq!(
            g.accessibility().and_then(|n| n.name).as_deref(),
            Some("Repeat")
        );
    }

    // Motion reads at its end: this test checks the resting frame.
    let _settled = crate::motion::testing::settled();
    nothing_chosen_until_the_person_picks();
    a_bound_selection_follows_its_state_both_ways();
    many_toggle_on_their_own_and_wrap();
    icon_segments_are_named_and_square();
    a_disabled_strip_takes_no_input();
    tabs_start_on_the_leading_edge_and_scroll_when_long();
    a_caption_names_the_strip_and_sits_above_it();
}

#[test]
fn a_multi_select_toggle_runs_its_action_with_the_chosen_indices() {
    use crate::widget::controls::ActionControls;
    let mut g = ToggleGroup::multiple(
        vec![
            Segment::new("Mon"),
            Segment::new("Tue"),
            Segment::new("Wed"),
        ],
        vec![true, false, false],
    )
    .with_id("days")
    .on_chosen_action(crate::widget::test_value_action_binding(
        tilo_actions::ParamType::Array,
    ));
    assert_eq!(
        g.control_action_bindings()
            .iter()
            .map(|binding| binding.control_id.as_str())
            .collect::<Vec<_>>(),
        ["days:segment:0", "days:segment:1", "days:segment:2"]
    );
    assert!(Widget::perform_semantic_action(
        &mut g,
        "days:segment:2",
        &crate::a11y::Action::ToggleCheck,
        &mut crate::widget::Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT,
    ));
    let invocation = g
        .take_control_invocation()
        .expect("the toggle runs the action");
    assert_eq!(
        invocation.arguments.get("value"),
        Some(&serde_json::json!([0, 2]))
    );
    assert_eq!(
        g.chosen().map(<[bool]>::to_vec),
        Some(vec![true, false, false]),
        "the strip shows the new set once the app hands it back"
    );
    g.refresh_control_action_availability(&std::collections::HashSet::new());
    assert!(
        !Widget::is_enabled(&g),
        "an unavailable action disables the strip"
    );
}
