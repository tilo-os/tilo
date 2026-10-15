//! Existing-event inspection uses the live selection, notes and action paths.
use super::*;
use automerge::ROOT;
use ui::headless::Framed;
use ui::synthetic_input::{char_key, cursor_moved, mouse_press, mouse_release};
use ui::widget::Edits;

fn mounted(width: u32, theme: Theme) -> Framed<CalendarApp> {
    let mut app = CalendarApp::new_demo();
    app.shots_show_calendar();
    app.view = View::Agenda;
    let state = ui::State::from_doc(super::tests::demo_document(&app));
    Framed::new(state, app, (width, 810), theme)
}

fn apply(app: &mut Framed<CalendarApp>, action: Action) {
    let (root, state) = Framed::split(app);
    root.apply_action(action, &mut Edits::lend(&mut state.doc));
    Framed::harness_mut(app)
        .recompose()
        .expect("calendar lays out");
}

fn activate(app: &mut Framed<CalendarApp>, target: &str) {
    let (root, state) = Framed::split(app);
    assert!(root.perform_semantic_action(
        target,
        &A11yAction::Activate,
        &mut Edits::lend(&mut state.doc),
        &ROOT
    ));
    Framed::harness_mut(app)
        .recompose()
        .expect("calendar lays out");
}

fn click(app: &mut Framed<CalendarApp>, point: Point) {
    for event in [
        cursor_moved(point.x, point.y),
        mouse_press(),
        mouse_release(),
    ] {
        Framed::harness_mut(app).dispatch_to_root_with(&event, &winit::event::Modifiers::default());
    }
}

fn has_name(node: &AccessNode, name: &str) -> bool {
    node.name.as_deref() == Some(name) || node.children.iter().any(|child| has_name(child, name))
}

#[test]
fn existing_selection_docks_right_with_immediately_visible_attached_notes() {
    for theme in [Theme::dark(), Theme::light().with_ui_scale(1.5)] {
        let mut app = mounted(1440, theme);
        let id = app
            .model
            .events
            .iter()
            .find(|event| event.title.starts_with("Design review"))
            .unwrap()
            .id;
        apply(&mut app, Action::OpenEvent(id));
        let pane = app.inspector_rect().expect("existing event docks");
        assert_eq!(pane.x1, app.rect.x1);
        assert!((520.0..=540.0).contains(&pane.width()));
        assert_eq!(app.view_rect().x1, pane.x0);
        assert!(app.view_rect().width() >= 420.0);
        assert!(app.modal_capture().is_none());
        assert!(app.modal.is_none());
        assert!(app.calendar_content_interactive());
        assert!(!app.event_form_active());
        assert!(!app.title_input.is_focused());
        let notes = app.notes_region();
        assert!(
            notes.y0 < pane.y1 - 150.0,
            "notes are visible without scrolling through fields"
        );
        assert!(notes.x0 >= pane.x0 && notes.x1 <= pane.x1);
        assert_eq!(app.notes_origin.y, notes.y0);
        assert!((app.detail_body_w() - app.notes_w).abs() < 0.01);
        assert!((app.built_notes_w - app.notes_w).abs() < 0.51);
        assert_eq!(app.bound, Some(Sel::Event(id)));
        assert!(app.notes_editor.get_rect().intersect(notes).area() > 0.0);
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::OpenEvent(_)))
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::ToggleTask(_)))
        );
        assert!(
            app.hits
                .iter()
                .any(|(_, action)| matches!(action, Action::EditEvent))
        );
        let semantics = app.accessibility().unwrap();
        assert!(has_name(&semantics, "Attached notes"));
        assert!(has_name(&semantics, "Edit event"));
        assert!(
            !app.sidebar_controls().is_empty(),
            "inspection keeps navigation accessible"
        );
    }
}

#[test]
fn attached_notes_edit_the_selected_document_and_task_checkbox_stays_live() {
    let mut app = mounted(1440, Theme::dark());
    let id = app
        .model
        .events
        .iter()
        .find(|event| event.title.starts_with("Design review"))
        .unwrap()
        .id;
    apply(&mut app, Action::OpenEvent(id));
    let notes = app.notes_region();
    click(&mut app, Point::new(notes.x0 + 48.0, notes.y0 + 48.0));
    assert!(app.notes_editor.is_focused());
    Framed::harness_mut(&mut app).dispatch_to_root_with(
        &char_key("Attached note addition"),
        &winit::event::Modifiers::default(),
    );
    Framed::harness_mut(&mut app)
        .recompose()
        .expect("edited notes lay out");
    let doc = &Framed::harness(&app).state().doc;
    assert!(
        crate::resource::read_doc(doc)
            .notes_text(id)
            .contains("Attached note addition"),
        "saved notes: {:?}",
        crate::resource::read_doc(doc).notes_text(id)
    );
    let (task, point) = app
        .hits
        .iter()
        .find_map(|(rect, action)| match action {
            Action::ToggleTask(id) => Some((*id, rect.center())),
            _ => None,
        })
        .expect("agenda retains task checkboxes");
    let before = app.model.task(task).unwrap().done;
    click(&mut app, point);
    assert_ne!(app.model.task(task).unwrap().done, before);
    assert_eq!(app.selected_event(), Some(id));
    assert!(app.inspector_rect().is_some());
    let doc = &Framed::harness(&app).state().doc;
    assert_eq!(
        crate::resource::read_doc(doc).task(task).unwrap().done,
        !before
    );
}

#[test]
fn edit_event_opens_every_field_and_done_closes_without_replacing_notes() {
    let mut app = mounted(1440, Theme::dark());
    let id = app.model.events[0].id;
    apply(&mut app, Action::OpenEvent(id));
    let before = crate::resource::read_doc(&Framed::harness(&app).state().doc).notes_text(id);
    activate(&mut app, "calendar.event:edit");
    assert!(app.inspector_rect().is_none());
    assert!(app.modal_capture().is_some());
    assert!(app.event_form_active());
    assert_eq!(app.form_event, Some(id));
    for rect in [
        app.form.title,
        app.form.start_date,
        app.form.start_time,
        app.form.end_date,
        app.form.end_time,
        app.form.location,
        app.form.calendar,
        app.form.repeat,
        app.form.alert,
        app.form.timezone,
        app.form.colour,
        app.form.invite,
    ] {
        assert!(rect.area() > 0.0, "full edit retains every field");
    }
    assert_eq!(app.bound, Some(Sel::Event(id)));
    assert_eq!(
        crate::resource::read_doc(&Framed::harness(&app).state().doc).notes_text(id),
        before
    );
    activate(&mut app, "calendar.back");
    assert!(!app.detail_open);
    assert!(app.selection.is_none());
    assert!(app.modal_capture().is_none());
}

#[test]
fn narrow_selection_is_modal_and_widening_restores_inspection() {
    let mut app = mounted(720, Theme::dark().with_ui_scale(1.5));
    let id = app.model.events[0].id;
    app.open_item(&crate::actions::event_resource_id(id))
        .expect("resource open selects the event");
    Framed::harness_mut(&mut app)
        .recompose()
        .expect("narrow selection lays out");
    assert!(app.inspector_rect().is_none());
    assert!(app.modal_capture().is_some());
    assert!(app.event_form_active());
    assert!(app.form.invite.area() > 0.0);
    Framed::harness_mut(&mut app)
        .relayout_themed((1440, 810), Theme::dark().with_ui_scale(1.5))
        .expect("wide selection lays out");
    assert!(app.inspector_rect().is_some());
    assert!(app.modal_capture().is_none());
    assert!(!app.title_input.is_focused());
    activate(&mut app, "calendar.back");
    assert!(!app.detail_open);
}

#[test]
fn focusing_attached_notes_releases_the_task_entry() {
    let mut app = mounted(1440, Theme::dark());
    let id = app
        .model
        .events
        .iter()
        .find(|event| event.title == "Design review")
        .unwrap()
        .id;
    apply(&mut app, Action::View(View::Tasks));
    apply(&mut app, Action::OpenEvent(id));
    let entry = app.tasks.entry_rect().expect("task entry stays visible");
    click(&mut app, entry.center());
    assert!(app.tasks.entry_focused());
    let notes = app.notes_region();
    click(&mut app, Point::new(notes.x0 + 48.0, notes.y0 + 48.0));
    assert!(app.notes_editor.is_focused());
    assert!(!app.tasks.entry_focused());
}

#[test]
fn draft_sample_full_editor_and_task_editor_remain_modal() {
    let mut app = mounted(1440, Theme::dark());
    apply(&mut app, Action::NewEvent);
    assert!(app.fresh_draft.is_some());
    assert!(app.inspector_rect().is_none());
    assert!(app.modal_capture().is_some());
    assert!(app.title_input.is_focused());
    activate(&mut app, "calendar.back");
    let task = app.model.tasks[0].id;
    apply(&mut app, Action::OpenTask(task));
    assert!(app.modal_capture().is_some());
    assert!(app.form.task_check.area() > 0.0);
    assert!(app.inspector_rect().is_none());
    let demo = CalendarApp::new_demo();
    let state = ui::State::from_doc(super::tests::demo_document(&demo));
    let demo = Framed::new(state, demo, (1440, 810), Theme::dark());
    assert!(demo.modal_capture().is_some());
    assert!(demo.event_form_active());
    assert!(demo.form.invite.area() > 0.0);
}
