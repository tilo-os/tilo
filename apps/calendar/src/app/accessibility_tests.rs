//! Hand-painted children keep their semantic owner and offscreen navigation.
use super::*;
use ui::widget::Edits;

#[test]
fn offscreen_calendar_navigation_can_be_revealed_without_pointer_input() {
    let mut app = CalendarApp::new_demo();
    app.detail_open = false;
    app.layout_theme = Theme::dark().with_ui_scale(1.5);
    app.rect = Rect::new(0.0, 0.0, 1440.0, 500.0);
    app.relayout();
    let paint = |app: &mut CalendarApp| {
        app.draw_sidebar(&mut PaintCtx::new(
            &mut Scene::new(),
            Affine::IDENTITY,
            &mut TextRenderer::new(),
            &app.layout_theme.clone(),
        ))
    };
    let before = paint(&mut app);
    let (rect, action) = app
        .sidebar_controls()
        .iter()
        .find(|(rect, action)| {
            rect.y0 > app.rect.y1 && matches!(action, Action::SetCalendarVisible { .. })
        })
        .cloned()
        .expect("offscreen calendar control");
    assert!(!before.iter().any(|(hit, _)| *hit == rect));
    let (target, _, _, _) = app.a11y_control(&action).unwrap();
    assert!(
        app.accessibility()
            .unwrap()
            .children
            .iter()
            .any(|n| n.action_target.as_deref() == Some(&target))
    );
    assert!(app.perform_semantic_action(
        &target,
        &A11yAction::ScrollIntoView,
        &mut Edits::lend(&mut AutoCommit::new()),
        &automerge::ROOT
    ));
    let after = paint(&mut app);
    assert!(app.sidebar_scroll.offset_y() > 0.0);
    assert!(after.iter().any(|(_, action)| {
        app.a11y_control(action)
            .is_some_and(|(id, _, _, _)| id == target)
    }));
}

#[test]
fn notes_block_actions_are_forwarded_to_the_attached_editor() {
    fn duplicate(node: &AccessNode) -> Option<String> {
        node.action_target
            .as_ref()
            .filter(|id| id.starts_with("notes.block-duplicate:"))
            .cloned()
            .or_else(|| node.children.iter().find_map(duplicate))
    }
    let mut app = CalendarApp::new_demo();
    let mut doc = super::tests::demo_document(&app);
    app.sync(&doc, &automerge::ROOT);
    let target = duplicate(&app.notes_editor.accessibility().unwrap()).expect("note block action");
    let before = app.notes_editor.editor().blocks().len();
    assert!(app.perform_semantic_action(
        &target,
        &A11yAction::Activate,
        &mut Edits::lend(&mut doc),
        &automerge::ROOT
    ));
    app.sync(&doc, &automerge::ROOT);
    assert_eq!(app.notes_editor.editor().blocks().len(), before + 1);
}
