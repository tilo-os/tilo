//! The person's asks of their agent door reach the door the desktop's host
//! hands its session, and nobody else's do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, agent_door,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// A door that admits every ask and keeps what it admitted.
#[derive(Default)]
struct Door {
    admitted: Mutex<Vec<(InvocationId, String)>>,
}

impl Door {
    fn admitted(&self, id: &InvocationId) -> bool {
        self.admitted
            .lock()
            .unwrap()
            .iter()
            .any(|(admitted, _)| admitted == id)
    }
}

impl ActionHost for Door {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        self.admitted
            .lock()
            .unwrap()
            .push((request.invocation, request.target.address.action.clone()));
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Accepted,
        }))
    }

    fn observe(&self, _: &Caller, id: &InvocationId) -> Option<InvocationState> {
        self.admitted(id).then(|| {
            InvocationState::new(
                *id,
                InvocationUpdate::Item {
                    invocation: *id,
                    sequence: 1,
                    value: serde_json::json!({"grants": []}),
                },
            )
        })
    }

    fn next_change(&self, _: &Caller, id: &InvocationId) -> Option<Pending<()>> {
        self.admitted(id).then(|| Pending::ready(()))
    }

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionHostError>> {
        Pending::ready(Ok(()))
    }
}

fn ask(action: &str) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: InvocationId::new(),
        target: agent_door::target(action),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

fn answer(
    pending: Pending<Result<InvocationReceipt, ActionHostError>>,
) -> Result<InvocationReceipt, ActionHostError> {
    pending
        .wait_timeout(Duration::from_secs(1))
        .ok()
        .flatten()
        .expect("answered at once")
}

#[test]
fn the_persons_asks_reach_the_door_the_host_hands_the_session() {
    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");

    let early = answer(session.invoke(&person, ask(agent_door::DOOR)));
    assert!(
        matches!(early, Err(ActionHostError::Unavailable { .. })),
        "without a door the read says none answers: {early:?}"
    );

    let door = Arc::new(Door::default());
    session.serve_person_door(agent_door::PROVIDER, door.clone());
    let read = ask(agent_door::DOOR);
    let id = read.invocation;
    assert!(answer(session.invoke(&person, read)).is_ok());
    let state = session
        .observe(&person, &id)
        .expect("the door answers its read");
    assert!(matches!(state.update, InvocationUpdate::Item { .. }));
    assert!(
        session.next_change(&person, &id).is_some(),
        "the read is followed"
    );
    assert!(answer(session.invoke(&person, ask(agent_door::STOP))).is_ok());
    assert_eq!(door.admitted.lock().unwrap().len(), 2);

    // An agent the person runs keeps no say over the door, nor does an
    // invocation the door never admitted read through it.
    let agent = person.clone().delegated_to("claude-code");
    let refused = answer(session.invoke(&agent, ask(agent_door::STOP)));
    assert_eq!(refused.unwrap_err(), ActionHostError::UnknownAction);
    assert_eq!(door.admitted.lock().unwrap().len(), 2);
    assert!(session.observe(&person, &InvocationId::new()).is_none());
}

/// What a window shows its tiles as: a notice, which is all a test needs.
struct Notices;

impl crate::TileSource for Notices {
    fn tile(
        &mut self,
        _: &tilo_desktop_model::Workspace,
        tab: &tilo_desktop_model::Tab,
    ) -> ui::widget::TileHost {
        ui::widget::TileHost::new(ui::widget::TileContent::Notice {
            title: tab.title.clone(),
            detail: String::new(),
        })
    }
}

/// A window asking the desk to show how an agent or a computer connects
/// (Settings' "Connect an agent or computer") chooses the host action the
/// person's own card does, which the page carries out: one setup, wherever
/// it is opened. An agent acting for the person can't ask for it.
#[test]
fn a_window_asks_the_desk_for_the_persons_connect_setup() {
    use tilo_actions::remote::session_actions;
    let mut minter = tilo_desktop_model::IdMinter::new(7);
    let tree = crate::seed_tree(&mut minter, "Desk", Vec::new());
    let handle = crate::DesktopSurface::new(tree, 7, Box::new(Notices)).1;
    let (session, openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let connect = || InvokeRequest {
        target: session_actions::target(session_actions::SHOW_CONNECT),
        argument: InvokeArgument::Json(serde_json::Map::new()),
        ..ask(agent_door::DOOR)
    };

    let agent = person.clone().delegated_to("claude-code");
    assert_eq!(
        answer(session.invoke(&agent, connect())).unwrap_err(),
        ActionHostError::UnknownAction
    );
    assert!(answer(session.invoke(&person, connect())).is_ok());
    assert!(
        openings.open(&handle, |_, _| Ok(())),
        "the desktop takes the ask on its next frame"
    );
    assert_eq!(
        handle.take_host_choices(),
        [session_actions::CONNECT_AGENT.to_owned()],
        "the choice the person's card makes"
    );
}
