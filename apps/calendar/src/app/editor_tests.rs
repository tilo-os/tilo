//! Scaled inspector layout and interaction contracts.
use super::*;
use ui::Widget;

#[test]
fn wide_editor_shows_notes_beside_fields_and_narrow_editor_stacks_them() {
    for (width, scale, split) in [
        (1440.0, 1.5, true),
        (720.0, 1.5, false),
        (1440.0, 2.0, false),
    ] {
        let mut app = CalendarApp::new_demo();
        let theme = Theme::dark().with_ui_scale(scale);
        app.layout_theme = theme.clone();
        app.rect = Rect::new(0.0, 0.0, width, 810.0);
        app.relayout();
        let modal = app.modal.as_ref().unwrap();
        assert!(app.form.notes_label.width() > 0.0);
        assert!((app.detail_body_w() - app.notes_w).abs() < 0.01);
        if split {
            assert!(app.form.notes_label.x0 > app.form.title.x1);
            assert!(app.form.notes_label.y0 < app.form.start_date.y0);
            assert!(!app.notes_region().contains(app.form.title.center()));
        } else {
            assert_eq!(app.form.notes_label.x0, modal.body.x0);
            assert!(app.form.notes_label.y0 > app.form.invite.y1);
        }
        assert!(modal.actions.iter().all(|r| app.rect.contains(r.center())));
    }
}

#[test]
fn manually_hosted_editors_use_ui_scaled_type_and_modal_semantics() {
    let theme = Theme::dark().with_ui_scale(1.5).with_text_scale(1.25);
    let mut app = CalendarApp::new_demo();
    app.layout_theme = theme.clone();
    app.rect = Rect::new(0.0, 0.0, 1440.0, 810.0);
    let mut tree = TaffyTree::new();
    app.build_layout(&mut tree, &theme, &mut HashMap::new());
    assert!((app.notes_editor.editor().body_size() - 21.0).abs() < 0.01);
    app.relayout();
    let mut scene = Scene::new();
    let mut text = TextRenderer::new();
    app.draw_sheet(
        &mut scene,
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::empty(),
    );
    let semantics = app.accessibility().unwrap();
    let done = semantics
        .children
        .iter()
        .find(|n| n.action_target.as_deref() == Some("calendar.back"))
        .unwrap();
    assert_eq!(
        done.bounds.unwrap().to_rect(),
        app.modal.as_ref().unwrap().actions[0]
    );
    assert!(
        !semantics
            .children
            .iter()
            .any(|n| n.action_target.as_deref() == Some("calendar.next"))
    );
}
