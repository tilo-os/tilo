//! The menu an event opens: open it, then what its resource offers, each as the palette invokes it.

use super::*;
use crate::app::event_menu::EventAction;
use ui::widget::controls::ActionControls;

fn labels(app: &CalendarApp, at: Option<Point>) -> Vec<String> {
    app.event_menu(at)
        .iter()
        .filter_map(|entry| entry.item().label().map(str::to_owned))
        .collect()
}

#[test]
fn an_events_menu_opens_it_and_offers_what_its_resource_does() {
    let mut h = Harness::new(1280, 900);
    h.app.detail_open = false;
    h.app.view = View::Month;
    h.relayout();
    let event = h
        .app
        .model
        .events
        .iter()
        .find(|event| !event.all_day)
        .expect("the sample holds a timed event")
        .clone();
    let at = h
        .app
        .hits
        .iter()
        .rev()
        .find_map(|(rect, action)| {
            matches!(action, Action::OpenEvent(id) if *id == event.id).then_some(rect.center())
        })
        .expect("the event's chip is drawn in the month grid");

    let offered = labels(&h.app, Some(at));
    assert_eq!(
        offered.first().map(String::as_str),
        Some("Open"),
        "{offered:?}"
    );
    assert!(
        offered.iter().any(|label| label == "Delete event"),
        "the event's own verbs are offered: {offered:?}"
    );
    assert!(
        offered
            .iter()
            .any(|label| label.starts_with("Reschedule") && label.ends_with('…')),
        "a verb that asks for values says so: {offered:?}"
    );
    assert!(
        labels(&h.app, Some(Point::new(-50.0, -50.0))).is_empty(),
        "away from an event there is no event menu"
    );

    // A verb that asks for nothing is invoked at the address its resource
    // publishes, as the palette invokes it.
    let resource = crate::actions::event_resource_id(event.id);
    let published =
        <crate::model::Calendar as ui::tilo_resource::Resource>::child(&h.app.model, &resource)
            .expect("the event's resource is published");
    let address = published
        .action_address("delete-event")
        .expect("the event's resource publishes delete-event");
    h.app.act_on_event(EventAction {
        address: address.clone(),
        label: event.title.clone(),
        asks: false,
    });
    let invoked = h
        .app
        .take_control_invocation()
        .expect("the row invokes its action");
    assert_eq!(invoked.address, address);

    // One that asks opens the palette on it, to ask for its values.
    let reschedule = published
        .action_address("reschedule")
        .expect("the event's resource publishes reschedule");
    h.app.act_on_event(EventAction {
        address: reschedule,
        label: event.title,
        asks: true,
    });
    assert!(
        h.app.take_control_invocation().is_some(),
        "the palette opens on the action"
    );
}
