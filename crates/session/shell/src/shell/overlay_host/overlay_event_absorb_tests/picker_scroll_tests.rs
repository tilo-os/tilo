use super::*;
use ui::widget::{PrimaryScrollAction, PrimaryScrollResult};

fn search(h: &mut AbsorbHarness) {
    for c in "thumbs up".chars() {
        assert!(pump(h, &key_pressed(Key::Character(c.to_string().into()))));
    }
    assert_eq!(h.host.emoji_picker().selected(), Some("👍"));
}

#[test]
fn native_picker_scroll_actions_cancel_old_pointer_and_keep_keyboard_focus() {
    let mut h = build_harness_sized(320.0, 240.0);
    h.host
        .layers
        .emoji_picker
        .open(Rect::new(0.0, 0.0, 320.0, 240.0), "en-US");
    search(&mut h);
    let theme = Theme::dark().with_text_scale(2.0);
    settle_with_theme(&mut h, 320.0, 240.0, &theme);
    let metrics = Widget::primary_vertical_scroll(&h.host)
        .expect("native picker scroll door")
        .metrics;
    assert!(metrics.content > metrics.viewport);
    let target = h
        .host
        .emoji_picker()
        .emoji_rect(0)
        .expect("visible selected target")
        .center();
    assert!(pump(&mut h, &cursor_moved_at(target)));
    assert!(pump(&mut h, &mouse_press()));
    assert!(matches!(
        Widget::apply_primary_vertical_scroll(&mut h.host, PrimaryScrollAction::End),
        PrimaryScrollResult::Changed(_)
    ));
    assert!(pump(&mut h, &mouse_release()));
    assert!(
        h.host.emoji_picker().is_open(),
        "late release must not insert"
    );
    assert!(
        h.log.borrow().is_empty(),
        "picker events must not reach content behind it"
    );
    let _ = Widget::apply_primary_vertical_scroll(&mut h.host, PrimaryScrollAction::Start);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::ArrowRight))));
    let _ = paint_scene(&mut h, &theme);
    assert!(h.host.emoji_picker().emoji_rect(0).is_some());
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.emoji_picker().is_open());
}

#[test]
fn native_tone_scroll_routes_above_the_body_and_nested_escape_stays_owned() {
    let mut h = build_harness_sized(240.0, 240.0);
    h.host
        .layers
        .emoji_picker
        .open(Rect::new(0.0, 0.0, 240.0, 240.0), "en-US");
    search(&mut h);
    let theme = Theme::light().with_ui_scale(1.5).with_text_scale(1.3);
    settle_with_theme(&mut h, 240.0, 240.0, &theme);
    assert!(pump_with_modifiers(
        &mut h,
        &key_pressed(Key::Named(NamedKey::Enter)),
        winit::event::Modifiers::from(winit::keyboard::ModifiersState::SHIFT)
    ));
    let _ = paint_scene(&mut h, &theme);
    assert!(h.host.emoji_picker().tone_forms().is_some());
    let metrics = Widget::primary_vertical_scroll(&h.host)
        .expect("native tone scroll door")
        .metrics;
    assert!(metrics.content > metrics.viewport);
    assert!(matches!(
        Widget::apply_primary_vertical_scroll(&mut h.host, PrimaryScrollAction::End),
        PrimaryScrollResult::Changed(_)
    ));
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::End))));
    let _ = paint_scene(&mut h, &theme);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(h.host.emoji_picker().is_open() && h.host.emoji_picker().tone_forms().is_none());
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.emoji_picker().is_open());
    assert!(
        h.log.borrow().is_empty(),
        "nested cancellation must stay in the foreground picker"
    );
}
