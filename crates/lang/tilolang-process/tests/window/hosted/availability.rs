//! What a window draws of an action another app publishes as unable to run now: its control
//! stands disabled, with that reason, and enables the moment the catalogue says it can run —
//! though the window keeps no resource of the app that publishes it.

use collaboration::CollabEvent;

use super::*;

/// What the window asked the catalogue, once it has asked at all: each correlation its answer
/// comes under, and the query.
fn asked(session: &Session) -> Vec<(u64, ui::tilo_actions::remote::CatalogueQuery)> {
    let asks: Vec<_> = session
        .sent
        .borrow()
        .iter()
        .filter_map(|request| match request {
            CollabRequest::RemoteAction {
                message:
                    ui::tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                        correlation,
                        query,
                    },
                ..
            } => Some((*correlation, query.clone())),
            _ => None,
        })
        .collect();
    assert!(
        !asks.is_empty(),
        "the window asks the catalogue what cannot run now"
    );
    asks
}

/// The catalogue's answer for the sound's `volume-up`: the action as the device publishes it,
/// `reason` when it cannot run now and `None` when it can.
fn page(reason: Option<&str>) -> ui::tilo_actions::remote::CataloguePage {
    use ui::tilo_actions::{ActionSpec, Availability, EncodedAction, OperationKind, ResourceSpec};
    let mut action = ActionSpec::typed_operation(
        "volume-up",
        "Volume up",
        "Turn the volume up",
        OperationKind::Effect,
        "so.tilo.device",
        Vec::new(),
        |_| {
            EncodedAction::encode_operation("so.tilo.device", &serde_json::json!({}))
                .expect("an encoded call")
        },
    );
    if let Some(reason) = reason {
        action.type_mut().contract.availability = Availability::Unavailable {
            code: "unavailable".to_owned(),
            reason: reason.to_owned(),
        };
    }
    let spec = ResourceSpec::new("sound", "sound", "Sound").with_action(action);
    ui::tilo_actions::remote::CataloguePage::published(
        "so.tilo.device",
        &[spec],
        &ui::tilo_actions::remote::CatalogueQuery::default(),
    )
}

/// Answer every ask the window made with `catalogue`, as the device's dispatch does.
fn answer(session: &Session, catalogue: &ui::tilo_actions::remote::CataloguePage) {
    let mut inbox = session.inbox().borrow_mut();
    for (correlation, _) in asked(session) {
        inbox.push_back(CollabEvent::RemoteAction {
            message: ui::tilo_actions::remote::RemoteActionMessage::CatalogueResponse {
                correlation,
                result: Ok(catalogue.clone()),
            },
        });
    }
}

/// The node the window shows for its one control.
fn volume_up(session: &mut Session) -> (ui::accesskit::NodeId, ui::accesskit::Node) {
    session
        .window
        .node_named("Volume up")
        .expect("the window shows the control")
}

#[test]
fn a_control_over_another_apps_action_draws_what_the_catalogue_says_of_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = Session::new(SOUND, "ada");
    let (_, query) = asked(&session)
        .into_iter()
        .next()
        .expect("the window asks the catalogue");
    assert!(
        query.unavailable_only,
        "the window asks the catalogue for the actions that cannot run now: {query:?}"
    );

    // The device says the volume cannot be turned up now: the control bound to that action stands
    // disabled, with the reason, before anyone presses it.
    answer(&session, &page(Some("the sound service is restarting")));
    session.settle();
    let (_, node) = volume_up(&mut session);
    assert!(node.is_disabled(), "the control cannot be pressed now");
    assert_eq!(
        node.description(),
        Some("the sound service is restarting"),
        "and it says why"
    );

    // The device publishes it can run again: the view runs again and the control enables, with no
    // reload.
    answer(&session, &page(None));
    session.settle();
    let (_, node) = volume_up(&mut session);
    assert!(!node.is_disabled(), "the control can be pressed again");
    assert_eq!(node.description(), None, "and says no reason");
}
