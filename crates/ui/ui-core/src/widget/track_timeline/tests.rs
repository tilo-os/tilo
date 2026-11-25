//! What each gesture on the timeline reports, where it lines up, and what a reader is told.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::float_cmp,
    reason = "in a test these are the assertion; the times compared are exact grid values"
)]

use winit::keyboard::{Key, ModifiersState, NamedKey};

use super::*;
use crate::a11y::Role;
use crate::component::{Described, Event, Fired, Intent, Value};
use crate::theme::Theme;
use crate::widget::{Ink, Widget};
use vello::kurbo::{Point, Rect};

fn area() -> Rect {
    Rect::new(0.0, 0.0, 1132.0, 400.0)
}

fn on(queue: &crate::Intents<Fired>, key: &str, event: &str) -> Event {
    Event::View(Intent::new(queue, key, event))
}

fn span(queue: &crate::Intents<Fired>, key: &str, start: f64, length: f64) -> TimelineSpanProps {
    TimelineSpanProps {
        key: key.into(),
        label: Some(key.to_uppercase()),
        start,
        length,
        tone: Ink::Accent,
        pressed: on(queue, key, "pressed"),
        moved: on(queue, key, "moved"),
        trimmed: on(queue, key, "trimmed"),
        ..TimelineSpanProps::default()
    }
}

fn track(
    queue: &crate::Intents<Fired>,
    key: &str,
    spans: Vec<TimelineSpanProps>,
) -> TimelineTrackProps {
    TimelineTrackProps {
        key: key.into(),
        label: Some(key.into()),
        spans,
        toggled_mute: on(queue, key, "toggled_mute"),
        toggled_lock: on(queue, key, "toggled_lock"),
        ..TimelineTrackProps::default()
    }
}

/// A cut of 20 seconds across 1000 pixels (50 a second) at 30 frames a second: V1 holds `a`
/// from 2 s to 6 s and `b` from 10 s to 14 s, A1 holds `c` from 0 s to 4 s, and a marker at 8 s.
fn cut(queue: &crate::Intents<Fired>) -> TrackTimelineProps {
    TrackTimelineProps {
        label: Some("The cut".into()),
        span: 20.0,
        playhead: 1.0,
        length: 14.0,
        step: 1.0 / 30.0,
        tracks: vec![
            track(
                queue,
                "v1",
                vec![span(queue, "a", 2.0, 4.0), span(queue, "b", 10.0, 4.0)],
            ),
            track(queue, "a1", vec![span(queue, "c", 0.0, 4.0)]),
        ],
        markers: vec![TimelineMarkerProps {
            key: "m".into(),
            label: Some("Beat".into()),
            at: 8.0,
            moved: on(queue, "m", "moved"),
            pressed: on(queue, "m", "pressed"),
            ..TimelineMarkerProps::default()
        }],
        scrub: on(queue, "cut", "scrub"),
        seek: on(queue, "cut", "seek"),
        zoomed: on(queue, "cut", "zoomed"),
        scrolled: on(queue, "cut", "scrolled"),
        press: on(queue, "cut", "press"),
        ..TrackTimelineProps::default()
    }
}

fn laid(props: TrackTimelineProps) -> TrackTimeline {
    let mut timeline = TrackTimeline::from_props("cut", props);
    timeline.layout_for(area(), &Theme::dark());
    timeline
}

fn row_y(timeline: &TrackTimeline, row: usize) -> f64 {
    timeline.geom_for_test().row(row).center().y
}

fn x(timeline: &TrackTimeline, t: f64) -> f64 {
    timeline.geom_for_test().x_of(t)
}

fn drag(timeline: &mut TrackTimeline, from: Point, to: Point) {
    assert!(timeline.press_at_for_test(from, ModifiersState::empty()));
    let mid = Point::new((from.x + to.x) / 2.0, (from.y + to.y) / 2.0);
    timeline.move_to_for_test(mid);
    timeline.move_to_for_test(to);
    timeline.release_at_for_test(to);
}

fn floats(value: Option<&Value>) -> Vec<f64> {
    match value {
        Some(Value::List(parts)) => parts
            .iter()
            .filter_map(|v| match v {
                Value::Float(f) => Some(*f),
                _ => None,
            })
            .collect(),
        Some(Value::Float(f)) => vec![*f],
        other => panic!("not numbers: {other:?}"),
    }
}

tilo_test_env::scenario! {
    fn a_track_timeline_reports_each_gesture_as_the_change_it_names;

    phase the_lane_is_fifty_pixels_a_second_right_of_the_headers;
    {
        let queue = crate::Intents::default();
        let timeline = laid(cut(&queue));
        let geom = timeline.geom_for_test();
        assert!((geom.px_per_second - 50.0).abs() < 1e-9, "{}", geom.px_per_second);
        assert_eq!(geom.x_of(0.0), geom.lane().x0);
        assert_eq!(geom.time_at(geom.x_of(7.5)), 7.5);
    }

    phase dragging_a_span_moves_it_and_lines_it_up_with_a_neighbour;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let y = row_y(&timeline, 0);
        // Grab `a` at 4 s, two seconds in, and let go so its end lands 3 px short of `b`'s start
        // at 10 s: it lines up with `b`.
        let to = x(&timeline, 8.0) - 3.0;
        let (from, to) = (Point::new(x(&timeline, 4.0), y), Point::new(to, y));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("a", "moved"));
        assert_eq!(
            fired[0].value,
            Some(Value::List(vec![Value::Float(6.0), Value::Text("v1".into())])),
            "its end met b's start"
        );
        // Until the view hands back where it stands, it is drawn where it was let go.
        assert_eq!(timeline.drawn_for_test("a"), Some((6.0, 4.0)));
    }

    phase a_free_drag_lands_on_the_frame_grid_and_another_track;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        // `b` from 12 s dragged down to A1, to 16.01 s: nothing near, so the nearest frame.
        let from = Point::new(x(&timeline, 12.0), row_y(&timeline, 0));
        let to = Point::new(x(&timeline, 18.01), row_y(&timeline, 1));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!(fired[0].event, "moved");
        let Some(Value::List(parts)) = &fired[0].value else {
            panic!("{fired:?}");
        };
        let Value::Float(start) = parts[0] else { panic!() };
        assert!(((start * 30.0) - (start * 30.0).round()).abs() < 1e-6, "on a frame: {start}");
        assert!((start - 16.0).abs() < 0.05, "{start}");
        assert_eq!(parts[1], Value::Text("a1".into()), "it moved onto A1");
    }

    phase dragging_an_edge_trims_the_span;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let y = row_y(&timeline, 0);
        // `a`'s trailing edge, at 6 s, dragged to the marker at 8 s.
        let (from, to) = (Point::new(x(&timeline, 6.0) - 2.0, y), Point::new(x(&timeline, 8.0) + 4.0, y));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("a", "trimmed"));
        let got = floats(fired[0].value.as_ref());
        assert!((got[0] - 2.0).abs() < 1e-9 && (got[1] - 6.0).abs() < 0.04, "{got:?}");
        // Its leading edge, at 2 s, dragged to the playhead at 1 s.
        let (from, to) = (Point::new(x(&timeline, 2.0) + 2.0, y), Point::new(x(&timeline, 1.0) + 3.0, y));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert_eq!(fired[0].event, "trimmed");
        let got = floats(fired[0].value.as_ref());
        assert_eq!(got, vec![1.0, 5.0], "it starts at the playhead, its end where it was");
    }

    phase a_press_on_a_span_carries_its_place_on_the_track;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let at = Point::new(x(&timeline, 11.0), row_y(&timeline, 0));
        assert!(timeline.press_at_for_test(at, ModifiersState::SHIFT));
        timeline.release_at_for_test(at);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("b", "pressed"));
        assert_eq!(fired[0].value, Some(Value::Int(1)), "b is the second span on V1");
    }

    phase the_ruler_seeks_and_a_drag_along_it_scrubs;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let ruler = timeline.geom_for_test().ruler();
        let y = ruler.y1 - 2.0;
        let (from, to) = (Point::new(x(&timeline, 3.0), y), Point::new(x(&timeline, 5.0), y));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert!(fired.iter().any(|f| f.event == "scrub"), "{fired:?}");
        let last = fired.last().unwrap();
        assert_eq!(last.event, "seek");
        assert_eq!(floats(last.value.as_ref()), vec![5.0]);
    }

    phase a_marker_drags_along_the_ruler;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let ruler = timeline.geom_for_test().ruler();
        let from = Point::new(x(&timeline, 8.0) + 1.0, ruler.y0 + 6.0);
        let (from, to) = (from, Point::new(x(&timeline, 12.5) + 1.0, ruler.y0 + 6.0));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("m", "moved"));
        assert!((floats(fired[0].value.as_ref())[0] - 12.5).abs() < 0.04);
    }

    phase a_locked_track_holds_its_spans_still;
    {
        let queue = crate::Intents::default();
        let mut props = cut(&queue);
        props.tracks[0].locked = true;
        let mut timeline = laid(props);
        let y = row_y(&timeline, 0);
        let (from, to) = (Point::new(x(&timeline, 4.0), y), Point::new(x(&timeline, 7.0), y));
        drag(&mut timeline, from, to);
        let fired = queue.take();
        assert!(
            fired.iter().all(|f| f.event == "pressed"),
            "a locked span is only pressed: {fired:?}"
        );
    }

    phase the_header_toggles_report_their_new_state;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let header = timeline.geom_for_test().header(1);
        // The lock stands at the trailing edge, the mute beside it (no solo is handled).
        let mute = Point::new(header.x1 - 6.0 - 22.0 - 2.0 - 11.0, header.y1 - 6.0 - 11.0);
        assert!(timeline.press_at_for_test(mute, ModifiersState::empty()));
        timeline.release_at_for_test(mute);
        assert_eq!(
            queue.take(),
            [crate::component::testing::fired("a1", "toggled_mute", Some(Value::Bool(true)))]
        );
    }

    phase keys_move_the_focus_nudge_a_span_and_step_the_playhead;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        // Focus starts at the playhead; down reaches V1's span nearest it, `a`.
        assert!(timeline.key_for_test(&Key::Named(NamedKey::ArrowDown), ModifiersState::empty()));
        assert!(timeline.key_for_test(&Key::Named(NamedKey::ArrowRight), ModifiersState::ALT));
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("a", "moved"));
        let got = floats(fired[0].value.as_ref());
        assert!((got[0] - (2.0 + 1.0 / 30.0)).abs() < 1e-9, "one frame later: {got:?}");
        assert!(timeline.key_for_test(&Key::Character(".".into()), ModifiersState::empty()));
        let fired = queue.take();
        assert_eq!(fired.last().unwrap().event, "seek");
        assert!((floats(fired.last().unwrap().value.as_ref())[0] - (1.0 + 1.0 / 30.0)).abs() < 1e-9);
    }

    phase zooming_keeps_the_moment_under_the_pointer;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let pointer = x(&timeline, 10.0);
        timeline.wheel_for_test(1.0, pointer);
        let fired = queue.take();
        let zoom = fired.iter().find(|f| f.event == "zoomed").expect("zoomed");
        assert_eq!(floats(zoom.value.as_ref()), vec![16.0]);
        assert!((timeline.geom_for_test().time_at(pointer) - 10.0).abs() < 1e-9);
        assert!(fired.iter().any(|f| f.event == "scrolled"));
    }

    phase a_pinch_scales_the_lane_about_the_pointer;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        let pointer = x(&timeline, 10.0);
        timeline.pinch_for_test(2.0, pointer);
        let fired = queue.take();
        let zoom = fired.iter().find(|f| f.event == "zoomed").expect("zoomed");
        assert_eq!(floats(zoom.value.as_ref()), vec![10.0], "twice as close");
        assert!((timeline.geom_for_test().time_at(pointer) - 10.0).abs() < 1e-9);
    }

    phase the_bar_under_the_tracks_scrolls_the_lane;
    {
        let queue = crate::Intents::default();
        let mut props = cut(&queue);
        // Forty seconds of cut in a lane that shows twenty: there is somewhere to scroll.
        props.length = 40.0;
        let mut timeline = laid(props);
        let (area, track) = timeline.scrollbar_for_test().expect("a bar under the tracks");
        assert!(
            area.y0 >= timeline.geom_for_test().row(1).y1 - 0.5,
            "the bar stands under the rows"
        );
        // The thumb starts at the leading end: take hold of it and drag a quarter of the track
        // along, which is about half of what it can travel.
        let grab = Point::new(track.x0 + 2.0, track.center().y);
        assert!(timeline.press_at_for_test(grab, ModifiersState::empty()));
        let to = Point::new(track.x0 + track.width() / 4.0, track.center().y);
        timeline.move_to_for_test(to);
        timeline.release_at_for_test(to);
        let fired = queue.take();
        let scrolled = fired
            .iter()
            .filter(|f| f.event == "scrolled")
            .next_back()
            .expect("the lane scrolled");
        let at = floats(scrolled.value.as_ref())[0];
        assert!((at - 10.0).abs() < 1.0, "a quarter of the track is half the cut: {at}");
        // A press past the thumb pages by what the lane shows.
        let after = Point::new(track.x1 - 4.0, track.center().y);
        assert!(timeline.press_at_for_test(after, ModifiersState::empty()));
        timeline.release_at_for_test(after);
        let fired = queue.take();
        let paged = fired
            .iter()
            .filter(|f| f.event == "scrolled")
            .next_back()
            .expect("a page");
        assert!(floats(paged.value.as_ref())[0] > at, "a page goes on from where it stood");
    }

    phase shift_on_an_empty_stretch_keeps_the_selection;
    {
        let queue = crate::Intents::default();
        let mut timeline = laid(cut(&queue));
        // V1 after `b` (which ends at 14 s) is empty.
        let empty = Point::new(x(&timeline, 16.0), row_y(&timeline, 0));
        assert!(timeline.press_at_for_test(empty, ModifiersState::SHIFT));
        timeline.release_at_for_test(empty);
        assert!(queue.take().is_empty(), "Shift adds nothing and clears nothing");
        let at = Point::new(x(&timeline, 16.0), row_y(&timeline, 0));
        assert!(timeline.press_at_for_test(at, ModifiersState::empty()));
        timeline.release_at_for_test(at);
        let fired = queue.take();
        assert_eq!(fired.len(), 1, "{fired:?}");
        assert_eq!((fired[0].key.as_str(), fired[0].event.as_str()), ("cut", "press"));
    }

    phase a_reader_hears_the_playhead_the_tracks_and_their_spans;
    {
        let queue = crate::Intents::default();
        let timeline = laid(cut(&queue));
        let root = timeline.accessibility().unwrap();
        assert_eq!(root.name.as_deref(), Some("The cut"));
        let playhead = &root.children[0];
        assert_eq!(playhead.role, Role::Slider);
        assert_eq!(playhead.value.as_deref(), Some("0:01.0"));
        let v1 = root
            .children
            .iter()
            .find(|n| n.name.as_deref() == Some("v1"))
            .expect("V1 is a group");
        let spans: Vec<_> = v1
            .children
            .iter()
            .filter(|n| n.role == Role::Button)
            .map(|n| (n.name.clone().unwrap_or_default(), n.value.clone().unwrap_or_default()))
            .collect();
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].0, "A");
        assert!(spans[0].1.contains("0:02.0") && spans[0].1.contains("0:06.0"), "{spans:?}");
        assert!(
            v1.children.iter().any(|n| n.role == Role::ToggleButton),
            "its mute and lock toggles"
        );
    }

    phase ten_thousand_spans_name_only_the_ones_in_view;
    {
        let queue = crate::Intents::default();
        let mut props = cut(&queue);
        props.tracks[0].spans = (0..10_000)
            .map(|i| span(&queue, &format!("s{i}"), f64::from(i) * 2.0, 1.5))
            .collect();
        let timeline = laid(props);
        let root = timeline.accessibility().unwrap();
        let v1 = root.children.iter().find(|n| n.name.as_deref() == Some("v1")).unwrap();
        let named = v1.children.iter().filter(|n| n.role == Role::Button).count();
        assert!((10..=12).contains(&named), "{named}");
    }
}

#[test]
fn the_ruler_keeps_its_labels_apart_at_every_zoom() {
    for px_per_second in [0.01, 0.5, 3.0, 50.0, 400.0, 2000.0] {
        let (major, minor) = tick_steps(px_per_second);
        assert!(
            major * px_per_second >= 72.0 || major == 7200.0,
            "{px_per_second}: {major}"
        );
        assert!(minor < major && minor > 0.0);
    }
    assert_eq!(timecode(65.5, true), "1:05.5");
    assert_eq!(timecode(3661.0, false), "1:01:01");
    assert_eq!(on_grid(1.016, 1.0 / 30.0), 1.0);
    assert_eq!(snap_target(5.1, &[0.0, 5.0, 6.0], 0.2), Some(5.0));
    assert_eq!(snap_target(5.5, &[0.0, 5.0, 6.0], 0.2), None);
}
