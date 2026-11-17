//! The person's asks of their MCP server reach the action API the desktop's host
//! hands its session, and nobody else's do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, mcp_server,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// An action host that admits every ask and keeps what it admitted.
#[derive(Default)]
struct Host {
    admitted: Mutex<Vec<(InvocationId, String)>>,
}

impl Host {
    fn admitted(&self, id: &InvocationId) -> bool {
        self.admitted
            .lock()
            .unwrap()
            .iter()
            .any(|(admitted, _)| admitted == id)
    }
}

impl ActionHost for Host {
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
        target: mcp_server::target(action),
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
fn the_persons_asks_reach_the_api_the_host_hands_the_session() {
    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");

    let early = answer(session.invoke(&person, ask(mcp_server::SERVER)));
    assert!(
        matches!(early, Err(ActionHostError::Unavailable { .. })),
        "without an API the read says none answers: {early:?}"
    );

    let api = Arc::new(Host::default());
    session.serve_person_api(mcp_server::PROVIDER, api.clone());
    let read = ask(mcp_server::SERVER);
    let id = read.invocation;
    assert!(answer(session.invoke(&person, read)).is_ok());
    let state = session
        .observe(&person, &id)
        .expect("the API answers its read");
    assert!(matches!(state.update, InvocationUpdate::Item { .. }));
    assert!(
        session.next_change(&person, &id).is_some(),
        "the read is followed"
    );
    assert!(answer(session.invoke(&person, ask(mcp_server::STOP))).is_ok());
    assert_eq!(api.admitted.lock().unwrap().len(), 2);

    // An agent the person runs keeps no say over the API, nor does an
    // invocation the API never admitted read through it.
    let agent = person.clone().delegated_to("claude-code");
    let refused = answer(session.invoke(&agent, ask(mcp_server::STOP)));
    assert_eq!(refused.unwrap_err(), ActionHostError::UnknownAction);
    assert_eq!(api.admitted.lock().unwrap().len(), 2);
    assert!(session.observe(&person, &InvocationId::new()).is_none());
}

/// What a protected field in a window hands the person's vault reaches the
/// API the host hands the session, asked by the person and by nobody acting
/// for them: not an agent they run, not an app.
#[test]
fn the_persons_vault_is_asked_by_the_person_alone() {
    use tilo_actions::remote::person_vault;

    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let adding = || InvokeRequest {
        target: person_vault::target(person_vault::ADD),
        ..ask(mcp_server::STOP)
    };
    let early = answer(session.invoke(&person, adding()));
    assert!(
        matches!(early, Err(ActionHostError::Unavailable { .. })),
        "without an API nothing keeps it: {early:?}"
    );

    let api = Arc::new(Host::default());
    session.serve_person_api(person_vault::PROVIDER, api.clone());
    assert!(answer(session.invoke(&person, adding())).is_ok());
    assert_eq!(api.admitted.lock().unwrap().len(), 1);

    let agent = person.delegated_to("helper");
    let app = Caller::local_app("so.tilo.chat")
        .acting_for("ada")
        .in_workspace("workspace");
    for other in [agent, app] {
        let refused = answer(session.invoke(&other, adding()));
        assert_eq!(refused.unwrap_err(), ActionHostError::UnknownAction);
    }
    assert_eq!(api.admitted.lock().unwrap().len(), 1);
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

/// A window asking the shell to show how an agent or a computer connects
/// (Settings' "Connect an agent or computer") chooses the host action the
/// person's own card does, which the page carries out: one setup, wherever
/// it is opened. An agent acting for the person can't ask for it.
#[test]
fn a_window_asks_the_shell_for_the_persons_connect_setup() {
    use tilo_actions::remote::session_actions;
    let mut minter = tilo_desktop_model::IdMinter::new(7);
    let tree = crate::seed_tree(&mut minter, "Office", Vec::new());
    let handle = crate::DesktopSurface::new(tree, 7, Box::new(Notices)).1;
    let (session, openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let connect = || InvokeRequest {
        target: session_actions::target(session_actions::SHOW_CONNECT),
        argument: InvokeArgument::Json(serde_json::Map::new()),
        ..ask(mcp_server::SERVER)
    };

    let agent = person.clone().delegated_to("claude-code");
    assert_eq!(
        answer(session.invoke(&agent, connect())).unwrap_err(),
        ActionHostError::UnknownAction
    );
    assert!(answer(session.invoke(&person, connect())).is_ok());
    assert!(
        openings.open(&handle, |_, _| Ok(()), |_, _| None),
        "the desktop takes the ask on its next frame"
    );
    assert_eq!(
        handle.take_host_choices(),
        [session_actions::CONNECT_AGENT.to_owned()],
        "the choice the person's card makes"
    );
}
