//! The person's asks of their MCP server reach the action API the desktop's host
//! hands its session, and nobody else's do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
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

impl ActionProvider for Host {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
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

    fn cancel(&self, _: &Caller, _: &InvocationId) -> Pending<Result<(), ActionCallError>> {
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
        signed: None,
    }
}

fn answer(
    pending: Pending<Result<InvocationReceipt, ActionCallError>>,
) -> Result<InvocationReceipt, ActionCallError> {
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
        matches!(early, Err(ActionCallError::Unavailable { .. })),
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
    assert_eq!(refused.unwrap_err(), ActionCallError::UnknownAction);
    assert_eq!(api.admitted.lock().unwrap().len(), 2);
    assert!(session.observe(&person, &InvocationId::new()).is_none());
}

/// What a protected field in a window hands the vault reaches the
/// workspace's server, asked by the person and by nobody acting for them: not
/// an agent of theirs, not an app. No device of theirs is asked.
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
        matches!(early, Err(ActionCallError::Unavailable { .. })),
        "a desktop in no workspace on a server keeps nothing: {early:?}"
    );

    let api = Arc::new(Host::default());
    session.serve_workspace_api(api.clone());
    assert!(answer(session.invoke(&person, adding())).is_ok());
    assert_eq!(api.admitted.lock().unwrap().len(), 1);

    let agent = person.delegated_to("helper");
    let app = Caller::local_app("so.tilo.chat")
        .acting_for("ada")
        .in_workspace("workspace");
    for other in [agent, app] {
        let refused = answer(session.invoke(&other, adding()));
        assert_eq!(refused.unwrap_err(), ActionCallError::UnknownAction);
    }
    assert_eq!(api.admitted.lock().unwrap().len(), 1);
}

/// What a person was told, and what they call important, is asked of the
/// workspace's server by the person alone, as the vault is: an agent and an
/// app on the device reach neither through the session.
#[test]
fn what_a_person_was_told_is_asked_by_the_person_alone() {
    use tilo_actions::remote::person_telling;

    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let listing = || InvokeRequest {
        target: person_telling::target(person_telling::TOLD, person_telling::LIST),
        ..ask(mcp_server::STOP)
    };
    let early = answer(session.invoke(&person, listing()));
    assert!(
        matches!(early, Err(ActionCallError::Unavailable { .. })),
        "a desktop in no workspace on a server answers nothing: {early:?}"
    );

    let api = Arc::new(Host::default());
    session.serve_workspace_api(api.clone());
    assert!(answer(session.invoke(&person, listing())).is_ok());
    assert_eq!(api.admitted.lock().unwrap().len(), 1);

    let agent = person.delegated_to("helper");
    let app = Caller::local_app("so.tilo.chat")
        .acting_for("ada")
        .in_workspace("workspace");
    for other in [agent, app] {
        let refused = answer(session.invoke(&other, listing()));
        assert_eq!(refused.unwrap_err(), ActionCallError::UnknownAction);
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
/// (Settings' "Connect an agent or computer"), or the link another of the
/// person's devices joins with (its "Add a device"), chooses the host action
/// the person's own card or search does, which the page carries out: one
/// setup and one pairing sheet, wherever they are opened. An agent acting
/// for the person can't ask for either.
#[test]
fn a_window_asks_the_shell_for_the_persons_connect_setup_or_pairing_link() {
    use tilo_actions::remote::session_actions;
    for (shown, chosen) in [
        (
            session_actions::SHOW_CONNECT,
            session_actions::CONNECT_AGENT,
        ),
        (
            session_actions::SHOW_PAIR_DEVICE,
            session_actions::PAIR_DEVICE,
        ),
    ] {
        let mut minter = tilo_desktop_model::IdMinter::new(7);
        let tree = crate::seed_tree(&mut minter, "Office", Vec::new());
        let handle = crate::DesktopSurface::new(tree, 7, Box::new(Notices)).1;
        let (session, openings) = super::serve("workspace", Arc::default());
        let person = Caller::local_person("ada").in_workspace("workspace");
        let show = || InvokeRequest {
            target: session_actions::target(shown),
            argument: InvokeArgument::Json(serde_json::Map::new()),
            ..ask(mcp_server::SERVER)
        };

        let agent = person.clone().delegated_to("claude-code");
        assert_eq!(
            answer(session.invoke(&agent, show())).unwrap_err(),
            ActionCallError::UnknownAction,
            "{shown}"
        );
        assert!(answer(session.invoke(&person, show())).is_ok(), "{shown}");
        assert!(
            openings.open(&handle, |_, _| Ok(()), |_, _| None),
            "the desktop takes the ask on its next frame"
        );
        assert_eq!(
            handle.take_host_choices(),
            [chosen.to_owned()],
            "{shown}: the choice the person's own card or search makes"
        );
    }
}

/// Model providers, mail accounts and the person's account with the server
/// are the workspace server's to keep: a window managing them reaches that
/// server alone, and the person's own
/// devices serving nothing changes nothing.
#[test]
fn workspace_accounts_and_models_reach_the_workspace_host_with_no_device() {
    use tilo_actions::remote::{person_accounts, person_models, server_account};

    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let server = Arc::new(Host::default());
    session.serve_workspace_api(server.clone());
    for target in [
        person_models::target(person_models::LIST),
        person_models::target(person_models::ADD_PRESET),
        person_accounts::target(person_accounts::WATCH),
        server_account::target(server_account::ACCOUNT),
    ] {
        let request = InvokeRequest {
            target,
            ..ask(mcp_server::STOP)
        };
        let id = request.invocation;
        assert!(answer(session.invoke(&person, request)).is_ok());
        assert!(server.admitted(&id), "the server admitted it");
        assert!(
            session.observe(&person, &id).is_some(),
            "and answers for it"
        );
    }
}

#[test]
fn a_person_reviews_proposals_on_the_workspace_host() {
    use tilo_actions::remote::workspace_initiative as initiative;

    let (session, _) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let delegated = person.clone().delegated_to("assistant");
    let server = Arc::new(Host::default());
    session.serve_workspace_api(server.clone());
    for action in [
        initiative::LIST,
        initiative::ACCEPT,
        initiative::DECLINE,
        initiative::ASK_FIRST,
        initiative::PROPOSE_AGAIN,
    ] {
        let request = InvokeRequest {
            target: initiative::target(initiative::PROPOSALS, action),
            ..ask(action)
        };
        let invocation = request.invocation;
        let elsewhere = Caller::local_person("ada").in_workspace("elsewhere");
        assert_eq!(
            answer(session.invoke(&elsewhere, request.clone())),
            Err(ActionCallError::UnknownAction)
        );
        let continuation = InvokeRequest {
            on_behalf_of: Some(InvocationId::new()),
            ..request.clone()
        };
        assert_eq!(
            answer(session.invoke(&person, continuation)),
            Err(ActionCallError::UnknownAction)
        );
        assert_eq!(
            answer(session.invoke(&delegated, request.clone())),
            Err(ActionCallError::UnknownAction)
        );
        assert!(!server.admitted(&invocation));
        assert!(answer(session.invoke(&person, request)).is_ok());
        assert!(server.admitted(&invocation));
    }
    for target in [
        initiative::target(initiative::LOOK_ROUND, initiative::ACCEPT),
        initiative::target(initiative::PROPOSALS, initiative::PROPOSE),
    ] {
        let request = InvokeRequest {
            target,
            ..ask(initiative::ACCEPT)
        };
        let invocation = request.invocation;
        assert_eq!(
            answer(session.invoke(&person, request)),
            Err(ActionCallError::UnknownAction)
        );
        assert!(!server.admitted(&invocation));
    }
}
