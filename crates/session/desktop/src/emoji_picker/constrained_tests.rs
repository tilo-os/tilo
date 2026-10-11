use super::*;

fn send(picker: &mut EmojiPicker, key: Key) {
    picker.handle_event(
        &ui::synthetic_input::keyboard_input(ui::synthetic_input::key_event(
            key,
            None,
            ElementState::Pressed,
        )),
        1.0,
        ModifiersState::empty(),
        None,
        Instant::now(),
    );
}

fn painted(viewport: Rect, theme: &Theme) -> EmojiPicker {
    let mut picker = EmojiPicker::new();
    picker.open(viewport, "en-US");
    for c in "thumbs up".chars() {
        send(&mut picker, Key::Character(c.to_string().into()));
    }
    assert_eq!(picker.selected(), Some("👍"));
    picker.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        theme,
        &PresenceCtx::default(),
        viewport,
    );
    picker
}

fn contains(outer: Rect, inner: Rect) -> bool {
    inner.x0 >= outer.x0 && inner.y0 >= outer.y0 && inner.x1 <= outer.x1 && inner.y1 <= outer.y1
}

#[test]
fn short_output_keeps_the_picker_and_selected_target_visible() {
    for theme in [
        Theme::light(),
        Theme::dark().with_text_scale(2.0),
        Theme::light().with_ui_scale(1.5).with_text_scale(1.3),
    ] {
        let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);
        let picker = painted(viewport, &theme);
        assert!(
            contains(viewport, picker.geometry.card),
            "picker must stay inside the short output"
        );
        assert!(
            picker.geometry.grid.height() >= picker.geometry.cell,
            "fixed controls must not erase the emoji grid"
        );
        let selected = picker
            .emoji_rect(picker.selected)
            .expect("selected emoji needs a visible target");
        assert!(contains(picker.geometry.card, selected));
        assert!(selected.height() > 0.0);
    }
}

#[test]
fn narrow_tones_keep_measured_targets_inside_the_card() {
    let viewport = Rect::new(0.0, 0.0, 240.0, 360.0);
    let mut picker = painted(viewport, &Theme::light());
    assert!(picker.open_tones(picker.selected));
    let tones = picker.tones.as_ref().expect("tone chooser");
    assert!(
        contains(picker.geometry.card, tones.rect),
        "narrow tone chooser must wrap instead of leaving the card"
    );
    assert_eq!(tones.cells.len(), 6);
    for rect in &tones.cells {
        assert!(contains(tones.rect, *rect));
        assert_eq!(rect.width(), picker.geometry.cell);
        assert_eq!(rect.height(), picker.geometry.cell);
    }
}

#[test]
fn wrapped_tones_navigate_by_spatial_row_and_keep_the_named_target() {
    let viewport = Rect::new(0.0, 0.0, 240.0, 360.0);
    let mut picker = painted(viewport, &Theme::dark());
    assert!(picker.open_tones(picker.selected));
    let tones = picker.tones.as_ref().expect("tone chooser");
    let columns = usize_from_f64(
        (picker.geometry.card.width() - 2.0 * f64::from(picker.spacing.s1)) / picker.geometry.cell,
    )
    .max(1);
    assert!(columns < tones.forms.len());
    send(&mut picker, Key::Named(NamedKey::ArrowDown));
    assert_eq!(
        picker.tones.as_ref().unwrap().selected,
        columns,
        "Down must reach the next painted row"
    );
    send(&mut picker, Key::Named(NamedKey::ArrowUp));
    assert_eq!(picker.tones.as_ref().unwrap().selected, 0);
    send(&mut picker, Key::Named(NamedKey::End));
    let tones = picker.tones.as_ref().unwrap();
    let target = tones.cells.last().unwrap().center();
    let expected = tones.forms.last().unwrap().clone();
    for state in [ElementState::Pressed, ElementState::Released] {
        picker.handle_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
            1.0,
            ModifiersState::empty(),
            Some(target),
            Instant::now(),
        );
    }
    assert_eq!(picker.take_commits(), [expected]);
}

#[test]
fn shared_body_scrolling_retires_old_presses_and_reveals_keyboard_focus() {
    use ui::widget::{PrimaryScrollAction, PrimaryScrollResult};
    let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);
    let mut picker = painted(viewport, &Theme::dark().with_text_scale(2.0));
    let target = picker
        .emoji_rect(0)
        .expect("visible selected emoji")
        .center();
    picker.handle_event(
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        },
        1.0,
        ModifiersState::empty(),
        Some(target),
        Instant::now(),
    );
    assert!(picker.press.is_some());
    assert!(matches!(
        picker.apply_primary_vertical_scroll(PrimaryScrollAction::End),
        PrimaryScrollResult::Changed(_)
    ));
    assert!(picker.press.is_none());
    picker.handle_event(
        &WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        },
        1.0,
        ModifiersState::empty(),
        Some(target),
        Instant::now(),
    );
    assert!(
        picker.take_commits().is_empty(),
        "semantic scrolling must cancel the old pointer target"
    );
    assert!(matches!(
        picker.apply_primary_vertical_scroll(PrimaryScrollAction::Start),
        PrimaryScrollResult::Changed(_)
    ));
    send(&mut picker, Key::Named(NamedKey::ArrowRight));
    assert!(
        picker.emoji_rect(0).is_some(),
        "keyboard focus must restore the selected emoji to view"
    );
    assert!(matches!(
        picker.apply_primary_vertical_scroll(PrimaryScrollAction::Absolute(f64::NAN)),
        PrimaryScrollResult::Unsupported
    ));
    send(&mut picker, Key::Named(NamedKey::Escape));
    assert!(!picker.needs_animation_frame());
    assert!(picker.primary_vertical_scroll().is_none());
}

#[test]
fn short_wrapped_tones_scroll_through_the_same_accessible_surface() {
    use ui::widget::{PrimaryScrollAction, PrimaryScrollResult};
    let viewport = Rect::new(0.0, 0.0, 240.0, 240.0);
    let theme = Theme::light().with_ui_scale(1.5).with_text_scale(1.3);
    let mut picker = painted(viewport, &theme);
    assert!(picker.open_tones(picker.selected));
    let body_offset = picker.body_scroll.offset();
    let tones = picker.tones.as_ref().unwrap();
    assert!(contains(picker.geometry.card, tones.rect));
    let expected = tones.forms.last().unwrap().clone();
    let metrics = picker
        .primary_vertical_scroll()
        .expect("tone scroll door")
        .metrics;
    assert!(metrics.content > metrics.viewport);
    assert!(matches!(
        picker.apply_primary_vertical_scroll(PrimaryScrollAction::End),
        PrimaryScrollResult::Changed(_)
    ));
    assert_eq!(
        picker.body_scroll.offset(),
        body_offset,
        "tone actions must not scroll the background body"
    );
    send(&mut picker, Key::Named(NamedKey::End));
    let tones = picker.tones.as_ref().unwrap();
    let target = picker.screen_tone_rect(*tones.cells.last().unwrap());
    assert_eq!(target.height(), picker.geometry.cell);
    assert!(contains(picker.geometry.card, target));
    let access = picker.tones_accessibility().unwrap();
    let focused = access
        .children
        .iter()
        .find(|node| node.state.focused)
        .expect("accessible focused tone");
    let bounds = focused.bounds.unwrap();
    assert_eq!(
        Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1),
        target
    );
    for state in [ElementState::Pressed, ElementState::Released] {
        picker.handle_event(
            &WindowEvent::MouseInput {
                device_id: winit::event::DeviceId::dummy(),
                state,
                button: MouseButton::Left,
            },
            1.0,
            ModifiersState::empty(),
            Some(target.center()),
            Instant::now(),
        );
    }
    assert_eq!(picker.take_commits(), [expected]);
}

#[test]
fn reduced_motion_scrollbars_do_not_request_idle_frames() {
    let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);
    let mut theme = Theme::light().with_text_scale(2.0);
    theme.reduce_motion = true;
    let mut picker = painted(viewport, &theme);
    let _ = picker.apply_primary_vertical_scroll(ui::widget::PrimaryScrollAction::End);
    assert!(!picker.needs_animation_frame());
    picker.close();
    picker.open(Rect::new(0.0, 0.0, 800.0, 700.0), "en-US");
    picker.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &theme,
        &PresenceCtx::default(),
        Rect::new(0.0, 0.0, 800.0, 700.0),
    );
    assert_eq!(
        picker.body_scroll.offset(),
        Vec2::ZERO,
        "reopening must retire the constrained viewport offset"
    );
}

#[test]
fn empty_search_reveals_its_message_and_query_correction_restores_selection() {
    for theme in [
        Theme::light(),
        Theme::dark().with_text_scale(2.0),
        Theme::light().with_ui_scale(1.5).with_text_scale(1.3),
    ] {
        let viewport = Rect::new(0.0, 0.0, 320.0, 240.0);
        let mut picker = EmojiPicker::new();
        picker.open(viewport, "en-US");
        let query = "zzzz-no-emoji-results";
        for c in query.chars() {
            send(&mut picker, Key::Character(c.to_string().into()));
        }
        picker.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::default(),
            viewport,
        );
        assert!(picker.items.is_empty());
        assert!(picker.selected().is_none());
        let message_band = picker.geometry.grid.with_origin((
            picker.geometry.grid.x0 - picker.body_scroll.offset().x,
            picker.geometry.grid.y0 - picker.body_scroll.offset().y,
        ));
        assert!(
            contains(picker.body_scroll.content_rect(), message_band),
            "the painted no-results message must be visible without manual scrolling"
        );
        for _ in query.chars() {
            send(&mut picker, Key::Named(NamedKey::Backspace));
        }
        for c in "thumbs up".chars() {
            send(&mut picker, Key::Character(c.to_string().into()));
        }
        picker.draw(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &theme,
            &PresenceCtx::default(),
            viewport,
        );
        assert_eq!(picker.selected(), Some("👍"));
        let selected = picker
            .emoji_rect(picker.selected)
            .expect("corrected query target");
        assert!(contains(picker.geometry.card, selected));
        picker.close();
        assert!(picker.take_commits().is_empty());
    }
}
