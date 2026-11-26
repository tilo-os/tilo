//! The person's asks of their MCP server reach the action API the desktop's host
//! hands its session, and nobody else's do.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_action_services::mcp_server;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_actions::{InvocationId, InvocationUpdate};

/// The workspace's model providers, as the service that keeps them publishes their actions.
mod model_providers {
    tilo_resource::bindings!(
        provider = "tilo.models",
        surface = "../../core/workspace-records/models-surface.json"
    );
}

/// The workspace's accounts, as the service that keeps them publishes their actions.
mod workspace_accounts {
    tilo_resource::bindings!(
        provider = "tilo.accounts",
        surface = "../../core/workspace-records/accounts-surface.json"
    );
}

/// The workspace's vault, as the service that keeps it publishes its actions.
mod vault_actions {
    tilo_resource::bindings!(
        provider = "tilo.vault",
        surface = "../../core/workspace-records/vault-surface.json"
    );
}

/// An action host that admits every ask and keeps what it admitted.
#[derive(Default)]
pub struct Host {
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

pub fn ask(action: &str) -> InvokeRequest {
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

pub fn answer(
    pending: Pending<Result<InvocationReceipt, ActionCallError>>,
) -> Result<InvocationReceipt, ActionCallError> {
    pending
        .wait_timeout(Duration::from_secs(1))
        .ok()
        .flatten()
        .expect("answered at once")
}

#[test]
pub fn the_persons_asks_reach_the_api_the_host_hands_the_session() {
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
pub fn the_persons_vault_is_asked_by_the_person_alone() {
    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let adding = || InvokeRequest {
        target: vault_actions::vault::add(),
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
pub fn what_a_person_was_told_is_asked_by_the_person_alone() {
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

/// A window asking the shell to show how an agent or a computer connects
/// (Settings' "Connect an agent or computer"), or the link another of the
/// person's devices joins with (its "Add a device"), chooses the host action
/// the person's own card or search does, which the page carries out: one
/// setup and one pairing sheet, wherever they are opened. An agent acting
/// for the person can't ask for either.
#[test]
pub fn a_window_asks_the_shell_for_the_persons_connect_setup_or_pairing_link() {
    use tilo_action_services::session_actions;
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
        let tree = crate::desktop::seed_tree(&mut minter, "Office", Vec::new());
        let handle = crate::desktop::state::DesktopHandle::new(tree, 7);
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
pub fn workspace_accounts_and_models_reach_the_workspace_host_with_no_device() {
    use tilo_action_services::server_account;

    let (session, _openings) = super::serve("workspace", Arc::default());
    let person = Caller::local_person("ada").in_workspace("workspace");
    let server = Arc::new(Host::default());
    session.serve_workspace_api(server.clone());
    for target in [
        model_providers::models::list(),
        model_providers::models::add_preset(),
        workspace_accounts::accounts::watch(),
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
