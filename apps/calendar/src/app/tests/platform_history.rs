use super::*;

#[test]
fn native_event_edits_record_the_actual_resource_title_in_platform_history() {
    let app = CalendarApp::new_demo();
    let mut doc = demo_document(&app);
    doc.commit();
    let event = app.model.events[0].clone();
    assert!(!event.title.trim().is_empty());
    for (label, action) in [
        (
            "Update event",
            ui::tilo_resource::Invoke::from(
                Calendar::paths()
                    .events
                    .item(&crate::model::key(event.id))
                    .location
                    .set("Meeting room".into()),
            ),
        ),
        (
            "Delete event",
            Calendar::DELETE_EVENT.with(crate::model::key(event.id)),
        ),
    ] {
        let before = doc.get_changes(&[]).len();
        app.try_submit(&mut Edits::lend(&mut doc), action)
            .expect("the actual calendar dispatcher admits the edit");
        assert_eq!(doc.get_changes(&[]).len(), before + 1);
        let heads = doc.get_heads();
        let context = doc
            .get_change_by_hash(&heads[0])
            .and_then(|change| {
                change
                    .message()
                    .and_then(tilo_actions::ActionContext::from_change_message)
            })
            .expect("the committed edit carries platform History provenance");
        assert!(
            matches!(
                &context.what,
                Some(tilo_actions::Did::Verb(_) | tilo_actions::Did::Edit(_))
            ),
            "{label}: {context:?}"
        );
        let resource = context.resource.expect("the edit names the calendar");
        assert_eq!(resource.id, "calendar.calendar");
        let item = context.item.expect("and the event it changed");
        assert_eq!(item.id, event.id.to_string(), "{label}");
        assert_eq!(item.label, event.title, "{label}");
    }
    assert!(crate::resource::read_doc(&doc).event(event.id).is_none());
}
