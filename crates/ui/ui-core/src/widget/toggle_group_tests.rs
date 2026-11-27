use super::test_support::*;
use super::*;
use crate::testing::dispatch;
use tilo_ui_base::widget::Widget;

use tilo_ui_base::synthetic_input::cursor_moved;

use tilo_ui_base::synthetic_input::mouse_press;

#[test]
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

#[test]
fn the_thumb_slides_to_a_clicked_segment_and_lands_on_a_key() {
    use crate::time::testing::with_monotonic_time;
    use tilo_ui_base::tokens::MotionSource;
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
        .compute_layout(
            root,
            <taffy::Size<taffy::AvailableSpace> as taffy::prelude::TaffyMaxContent>::MAX_CONTENT,
        )
        .unwrap();
    g.apply_layout(&taffy, node, Point::ZERO);
    let mut tr = TextRenderer::new();
    let presence = tilo_ui_base::presence::PresenceCtx::default();
    let mut paint = |g: &mut ToggleGroup| {
        let mut scene = Scene::new();
        g.draw(&mut tilo_ui_base::draw::DrawCtx::new(
            &mut scene,
            Affine::IDENTITY,
            &mut tr,
            &theme,
            presence.draw_env(),
        ));
    };
    let t0 = crate::time::now();
    let at = |ms: u64| t0 + crate::time::Duration::from_millis(ms);
    with_monotonic_time(t0, || paint(&mut g));
    assert!(!g.needs_animation_frame(), "the first paint rests");
    let (first, last) = (g.segment_rects[0], g.segment_rects[2]);
    with_monotonic_time(t0, || {
        tilo_ui_base::motion::note_source(MotionSource::Pointer);
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
            tilo_ui_base::motion::input_source(),
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
        tilo_ui_base::motion::note_source(MotionSource::Keyboard);
        g.set_selected(0);
        paint(&mut g);
        assert!(!g.needs_animation_frame(), "a key's choice lands at once");
    });
}

#[test]
fn icon_segments_are_named_and_square() {
    let segments = vec![
        Segment::icon(tilo_ui_base::icons::Icon::List, "List"),
        Segment::icon(tilo_ui_base::icons::Icon::LayoutGrid, "Grid"),
    ];
    let mut g = ToggleGroup::new(segments, 0);
    let mut text = TextRenderer::new();
    g.measure_naturals(&mut text);
    assert!((g.naturals[0].0 - g.metrics.height.ceil()).abs() < 1e-9);
    let node = g.accessibility().expect("semantics");
    assert_eq!(node.children[1].name.as_deref(), Some("Grid"));
}
