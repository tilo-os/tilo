//! What an app may reach the door with as the person using it, besides what it owns: the
//! view lookup that mints a link to its own record, the reference actions that show another
//! resource, and the desk's invitation into the workspace it shows. None of them grants
//! anything, so the person's own part decides them, where invoking another app's action is
//! decided for the app.
//!
//! Sharing is the workspace's membership: an app shows something by inviting people into the
//! workspace it is in, and the invitation it asks for is the one the invite sheet makes.

use tilo_actions::remote::{APPLICATION_OPEN_ACTION, application_provider};

/// Whether `query` asks only where one of `app_id`'s own resources is shown,
/// as minting a link to it does. That is read-only view discovery of the
/// app's own record, answered as the person using it, never the catalogue of
/// what else they are offered, so it needs no declared `query` access.
pub(super) fn own_view_lookup(app_id: &str, query: &tilo_actions::remote::CatalogueQuery) -> bool {
    query.view_resource.is_some()
        && query
            .provider
            .as_deref()
            .and_then(|provider| provider.strip_prefix("record:"))
            .and_then(|record| record.split_once(':'))
            .is_some_and(|(app, _)| app == app_id)
}

/// Whether `request` asks for the link to a resource or opens it:
/// `share-link`, which the framework answers for every app and no app may
/// publish itself, or the resource's `open` in its app
/// ([`application_action_target`](tilo_actions::remote::application_action_target)),
/// which shows it. Neither grants anything, so an app showing a reference (a
/// chat message carrying a page, an issue's link) asks as the person using it
/// without declared `invoke` access; the person's own grants on the app and
/// the resource still decide.
pub(super) fn reference_request(request: &tilo_actions::remote::InvokeRequest) -> bool {
    let action = request.target.address.action.as_str();
    action == "share-link"
        || (action == APPLICATION_OPEN_ACTION
            && application_provider(&request.target.provider).is_some())
}

/// Whether `request` asks the session's desk for its invitation into the workspace — the sheet
/// where a link is made (`show-invite`) and the link itself (`issue-invitation`) — so an app
/// sharing what it shows offers the same sheet and the same invitation, decided by the person's
/// part there. It reaches nothing else of the desk — the surface up, the workspace in front, the
/// agents — and grants nothing: the invitation admits whoever redeems it, as the person's own
/// would.
pub(super) fn invitation_request(request: &tilo_actions::remote::InvokeRequest) -> bool {
    use tilo_actions::remote::session_actions;
    let action = request.target.address.action.as_str();
    request.target.provider == tilo_actions::remote::SESSION_ACTIONS_PROVIDER
        && request.target.address.resource == session_actions::DESK_RESOURCE
        && (action == session_actions::ISSUE_INVITATION || action == session_actions::SHOW_INVITE)
}

#[cfg(test)]
mod own_view_lookup_tests {
    use super::own_view_lookup;
    use tilo_actions::remote::{CatalogueQuery, ViewResourceQuery};

    #[test]
    fn an_app_may_look_up_only_its_own_resources_views() {
        let lookup = |provider: &str, view: bool| CatalogueQuery {
            provider: Some(provider.into()),
            view_resource: view.then(|| ViewResourceQuery {
                id: "notes.page:1".into(),
                kind: "notes.page".into(),
            }),
            ..CatalogueQuery::default()
        };
        assert!(own_view_lookup(
            "so.tilo.notes",
            &lookup("record:so.tilo.notes:", true)
        ));
        assert!(own_view_lookup(
            "so.tilo.notes",
            &lookup("record:so.tilo.notes:uuid:1", true)
        ));
        assert!(!own_view_lookup(
            "so.tilo.notes",
            &lookup("record:so.tilo.notes.evil:", true)
        ));
        assert!(!own_view_lookup(
            "so.tilo.chat",
            &lookup("record:so.tilo.notes:", true)
        ));
        assert!(
            !own_view_lookup("so.tilo.notes", &lookup("record:so.tilo.notes:", false)),
            "the action catalogue still needs declared access"
        );
    }

    #[test]
    fn only_linking_and_opening_a_resource_are_open_to_every_app() {
        let request = |provider: &str, action: &str| tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: provider.into(),
                address: tilo_actions::ActionAddress::new("notes.page:1", action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        };
        let record = "record:so.tilo.notes:";
        let app = "application:workspace-1:so.tilo.notes:uuid:1";
        assert!(super::reference_request(&request(record, "share-link")));
        assert!(!super::reference_request(&request(
            record,
            "share-invite-link"
        )));
        assert!(!super::reference_request(&request(record, "rename")));
        assert!(super::reference_request(&request(app, "open")));
        assert!(!super::reference_request(&request(app, "rename")));
        assert!(
            !super::reference_request(&request("7", "open")),
            "only the resource's own app opens it"
        );
    }
}

#[cfg(test)]
mod invitation_request_tests {
    use crate::collab_app_broker::AppBrokerState;
    use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices};
    use crate::test_support::{Workspace, WorkspaceSlot, WorkspaceTree};
    use std::sync::{Arc, Mutex};
    use tilo_actions::remote::session_actions;
    use tilo_actions::remote::{ActionHostError, Caller, Pending, RemoteActionMessage, pending};

    struct Desk {
        started: Mutex<Vec<String>>,
    }

    impl HostStepServices for Desk {
        fn provider(&self) -> &'static str {
            session_actions::PROVIDER
        }

        fn app_id(&self) -> &'static str {
            session_actions::PROVIDER
        }

        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            vec![tilo_actions::ActionDescriptor {
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(
                    session_actions::DESK_RESOURCE,
                    session_actions::ISSUE_INVITATION,
                ),
                resource_kind: "session.desk".into(),
                resource_label: "Desk".into(),
                action_label: "Make an invitation".into(),
                keywords: Vec::new(),
                icon: None,
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract::default(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            }]
        }

        fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
            self.started
                .lock()
                .unwrap()
                .push(request.address.action.clone());
            let (answer, pending) = pending();
            answer.complete(Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({
                    "invite": "ab".repeat(32),
                    "link": "tilo://join/ab",
                    "role": "editor",
                    "expires_at_ns": 0,
                })),
                resources: Vec::new(),
            }));
            Ok(pending)
        }
    }

    fn request(
        provider: &str,
        resource: &str,
        action: &str,
    ) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: tilo_actions::remote::RemoteActionTarget {
                provider: provider.into(),
                address: tilo_actions::ActionAddress::new(resource, action),
            },
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
        }
    }

    fn invitation() -> tilo_actions::remote::InvokeRequest {
        request(
            session_actions::PROVIDER,
            session_actions::DESK_RESOURCE,
            session_actions::ISSUE_INVITATION,
        )
    }

    #[test]
    fn an_app_asks_the_desk_for_the_invitation_and_nothing_else() {
        let desk = |action: &str| {
            request(
                tilo_actions::remote::SESSION_ACTIONS_PROVIDER,
                session_actions::DESK_RESOURCE,
                action,
            )
        };
        assert!(super::invitation_request(&desk(
            session_actions::ISSUE_INVITATION
        )));
        assert!(
            super::invitation_request(&desk(session_actions::SHOW_INVITE)),
            "the sheet an app shares through is the person's own invite sheet"
        );
        for other in [
            "invitations",
            "cancel-invitation",
            "switch-workspace",
            "show-surface",
            "stop-agent",
        ] {
            assert!(
                !super::invitation_request(&desk(other)),
                "the desk's `{other}` is the person's own"
            );
        }
        assert!(
            !super::invitation_request(&request(
                "record:so.tilo.notes:",
                session_actions::DESK_RESOURCE,
                session_actions::ISSUE_INVITATION,
            )),
            "an invitation is the desk's, not a resource's"
        );
        assert!(
            !super::invitation_request(&request(
                tilo_actions::remote::SESSION_ACTIONS_PROVIDER,
                "session:chrome",
                session_actions::ISSUE_INVITATION,
            )),
            "and it is the desk's resource that offers it"
        );
    }

    /// An app sharing what it shows asks the desk for the invitation as the person using it: the
    /// workspace's own grants decide, so a member who may only read is refused, and the desk is
    /// never asked to make one.
    #[test]
    fn an_apps_invitation_is_refused_for_a_member_who_may_only_read() {
        let owner = hex::encode([7; 32]);
        let viewer = hex::encode([8; 32]);
        let mut workspace = Workspace::new(WorkspaceSlot(96), "Shared folder");
        workspace.set_owner(owner.clone());
        workspace.admit(viewer.clone(), tilo_action_policy::Role::Viewer);
        let workspace_id = workspace.id().to_string();
        let mut tree = WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let desk = Arc::new(Desk {
            started: Mutex::default(),
        });
        let (state, _bridge) = AppBrokerState::test_state(
            Some(tree.shared().authority()),
            crate::services::HostServices {
                host_steps: vec![desk.clone()],
                ..Default::default()
            },
        );
        let app = "so.tilo.file-explorer";
        let ask = invitation();
        let door = |person: &Caller| {
            state
                .app_door(person, app, &RemoteActionMessage::Invoke(ask.clone()))
                .expect("the app reaches the desk's invitation as the person")
        };
        let invoke = |caller: &Caller| {
            state
                .action_invoke(caller, None, None, &ask)
                .wait_timeout(tilo_test_env::hang_bound())
                .ok()
                .flatten()
                .expect("admission answers at once")
                .map(|_| ())
        };

        let reading = Caller::local_person(viewer).in_workspace(workspace_id.clone());
        assert_eq!(
            invoke(&door(&reading)),
            Err(ActionHostError::NotPermitted),
            "a viewer makes no invitation"
        );
        assert!(desk.started.lock().unwrap().is_empty());

        let owning = Caller::local_person(owner).in_workspace(workspace_id);
        invoke(&door(&owning)).expect("the owner makes one");
        assert_eq!(
            *desk.started.lock().unwrap(),
            [session_actions::ISSUE_INVITATION]
        );
    }
}
