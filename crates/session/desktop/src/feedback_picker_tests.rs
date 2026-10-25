use super::*;

/// A window with a list of rows, each holding a button.
struct Screen;

fn target(key: &str, rect: Rect) -> PickTarget {
    PickTarget {
        key: key.into(),
        rect,
        caption: key.into(),
    }
}

fn row(index: u32) -> Rect {
    let y = 40.0 * f64::from(index);
    Rect::new(0.0, y, 400.0, y + 40.0)
}

impl PickSource for Screen {
    fn at(&self, point: Point) -> Vec<PickTarget> {
        let Some(index) = (0..10).find(|index| row(*index).contains(point)) else {
            return Vec::new();
        };
        let mut chain = vec![
            target("list", Rect::new(0.0, 0.0, 400.0, 400.0)),
            target(&format!("row{index}"), row(index)),
        ];
        let button = Rect::new(300.0, row(index).y0 + 5.0, 390.0, row(index).y1 - 5.0);
        if button.contains(point) {
            chain.push(target(&format!("button{index}"), button));
        }
        chain
    }

    fn within(&self, area: Rect, limit: usize) -> Vec<PickTarget> {
        (0..10)
            .filter(|index| area.contains_rect(row(*index)))
            .map(|index| target(&format!("row{index}"), row(index)))
            .take(limit)
            .collect()
    }
}

fn keys(targets: &[PickTarget]) -> Vec<&str> {
    targets.iter().map(|t| t.key.as_str()).collect()
}

fn opened() -> (FeedbackPicker, Instant) {
    let now = ui::time::now();
    let mut picker = FeedbackPicker::default();
    picker.open(now);
    (picker, now)
}

fn click(picker: &mut FeedbackPicker, point: Point, shift: bool, now: Instant) {
    picker.hover(point, &Screen, now);
    picker.button(point, true, shift, &Screen);
    picker.button(point, false, shift, &Screen);
}

#[test]
fn a_click_picks_the_deepest_part_under_the_pointer_and_ends() {
    let (mut picker, now) = opened();
    click(&mut picker, Point::new(320.0, 60.0), false, now);
    assert!(!picker.is_open());
    let Some(PickerOutcome::Picked(parts)) = picker.take_outcome() else {
        panic!("a click picks");
    };
    assert_eq!(keys(&parts), ["button1"]);
}

#[test]
fn the_wheel_widens_to_the_part_around_and_narrows_back() {
    let (mut picker, now) = opened();
    picker.hover(Point::new(320.0, 60.0), &Screen, now);
    picker.widen(true, ui::tokens::MotionSource::Pointer, now);
    picker.widen(true, ui::tokens::MotionSource::Pointer, now);
    picker.widen(true, ui::tokens::MotionSource::Pointer, now);
    // Moving within the same part keeps the widened level.
    picker.hover(Point::new(310.0, 61.0), &Screen, now);
    assert_eq!(picker.session.as_ref().unwrap().lit().unwrap().key, "list");
    picker.widen(false, ui::tokens::MotionSource::Pointer, now);
    assert_eq!(picker.session.as_ref().unwrap().lit().unwrap().key, "row1");
    picker.button(Point::new(310.0, 61.0), true, false, &Screen);
    picker.button(Point::new(310.0, 61.0), false, false, &Screen);
    assert_eq!(
        picker.take_outcome(),
        Some(PickerOutcome::Picked(vec![target("row1", row(1))]))
    );
}

#[test]
fn shift_clicks_and_a_dragged_rectangle_pick_several_until_continue() {
    let (mut picker, now) = opened();
    click(&mut picker, Point::new(10.0, 10.0), true, now);
    click(&mut picker, Point::new(320.0, 100.0), true, now);
    assert!(picker.is_open());
    assert_eq!(keys(picker.selected()), ["row0", "button2"]);
    // Shift-clicking a picked part puts it back.
    click(&mut picker, Point::new(10.0, 10.0), true, now);
    assert_eq!(keys(picker.selected()), ["button2"]);
    picker.button(Point::new(0.0, 155.0), true, false, &Screen);
    picker.hover(Point::new(400.0, 245.0), &Screen, now);
    picker.button(Point::new(400.0, 245.0), false, false, &Screen);
    assert_eq!(keys(picker.selected()), ["button2", "row4", "row5"]);
    assert!(picker.is_open(), "a drag adds without ending");
    picker.key(&Key::Named(NamedKey::Enter), now);
    let Some(PickerOutcome::Picked(parts)) = picker.take_outcome() else {
        panic!("Enter continues with what is picked");
    };
    assert_eq!(parts.len(), 3);
}

#[test]
fn picks_stop_at_the_reports_limit() {
    let (mut picker, now) = opened();
    picker.button(Point::new(0.0, 0.0), true, false, &Screen);
    picker.hover(Point::new(400.0, 400.0), &Screen, now);
    picker.button(Point::new(400.0, 400.0), false, false, &Screen);
    assert_eq!(picker.selected().len(), MAX_PICKS);
    click(&mut picker, Point::new(320.0, 380.0), true, now);
    assert_eq!(picker.selected().len(), MAX_PICKS);
}

#[test]
fn escape_leaves_without_a_pick_and_nothing_picked_is_a_cancel() {
    let (mut picker, now) = opened();
    click(&mut picker, Point::new(10.0, 10.0), true, now);
    assert!(picker.key(&Key::Named(NamedKey::Escape), now));
    assert_eq!(picker.take_outcome(), Some(PickerOutcome::Cancelled));
    let (mut picker, now) = opened();
    click(&mut picker, Point::new(900.0, 10.0), false, now);
    assert!(picker.is_open(), "a click on nothing picks nothing");
    picker.key(&Key::Named(NamedKey::Enter), now);
    assert_eq!(picker.take_outcome(), Some(PickerOutcome::Cancelled));
    assert!(!FeedbackPicker::default().key(&Key::Named(NamedKey::Escape), now));
}

#[test]
fn the_pointer_glides_the_highlight_and_then_asks_for_no_frames() {
    let (mut picker, now) = opened();
    picker.hover(Point::new(10.0, 10.0), &Screen, now);
    assert!(
        !picker.needs_animation_frame(now),
        "the first part lit lands"
    );
    picker.hover(Point::new(10.0, 90.0), &Screen, now);
    let glide = picker.session.as_ref().unwrap().glide.unwrap();
    assert_eq!(glide.get(now), row(0), "it starts where it was");
    let later = now + ui::time::Duration::from_millis(60);
    let between = glide.get(later);
    assert!(between.y0 > 0.0 && between.y0 < 80.0, "{between:?}");
    assert!(picker.needs_animation_frame(later));
    let settled = glide.settles_at().expect("it is moving");
    assert_eq!(glide.get(settled), row(2));
    assert!(!picker.needs_animation_frame(settled));
}

#[test]
fn a_key_lands_the_highlight_with_no_frames_asked() {
    let (mut picker, now) = opened();
    picker.hover(Point::new(350.0, 10.0), &Screen, now);
    let button = Rect::new(300.0, 5.0, 390.0, 35.0);
    assert_eq!(
        picker.session.as_ref().unwrap().glide.unwrap().get(now),
        button
    );
    picker.key(&Key::Named(NamedKey::ArrowUp), now);
    let glide = picker.session.as_ref().unwrap().glide.unwrap();
    assert_eq!(
        glide.get(now),
        row(0),
        "it is on the row the key widened to"
    );
    assert!(!glide.is_moving(now));
    assert!(!picker.needs_animation_frame(now));
}

#[test]
fn the_wheel_glides_the_highlight() {
    let (mut picker, now) = opened();
    picker.hover(Point::new(350.0, 10.0), &Screen, now);
    picker.widen(true, ui::tokens::MotionSource::Pointer, now);
    assert!(picker.needs_animation_frame(now));
}

#[test]
fn holes_inside_another_are_not_cut_twice() {
    let bounds = Rect::new(0.0, 0.0, 100.0, 100.0);
    let one = dimmed(bounds, &[Rect::new(10.0, 10.0, 50.0, 50.0)]);
    let nested = dimmed(
        bounds,
        &[
            Rect::new(10.0, 10.0, 50.0, 50.0),
            Rect::new(20.0, 20.0, 30.0, 30.0),
            Rect::new(10.0, 10.0, 50.0, 50.0),
        ],
    );
    assert_eq!(one.elements().len(), nested.elements().len());
}

fn dim(picker: &FeedbackPicker, now: Instant) -> f64 {
    picker.session.as_ref().unwrap().dim.progress(now)
}

#[test]
fn a_shortcut_dims_the_screen_at_once() {
    for source in [
        ui::tokens::MotionSource::Keyboard,
        ui::tokens::MotionSource::Action,
    ] {
        ui::motion::note_source(source);
        let (picker, now) = opened();
        assert!((dim(&picker, now) - 1.0).abs() < f64::EPSILON, "{source:?}");
        assert!(!picker.needs_animation_frame(now), "{source:?}");
    }
}

#[test]
fn a_click_fades_the_dim_in_and_then_asks_for_no_frames() {
    ui::motion::note_source(ui::tokens::MotionSource::Pointer);
    let (picker, now) = opened();
    assert!(dim(&picker, now) < 0.01);
    assert!(picker.needs_animation_frame(now));
    let between = dim(&picker, now + ui::time::Duration::from_millis(60));
    assert!(between > 0.0 && between < 1.0, "{between}");
    let settled = now + ui::time::Duration::from_secs(1);
    assert!((dim(&picker, settled) - 1.0).abs() < f64::EPSILON);
    assert!(!picker.needs_animation_frame(settled));
}
