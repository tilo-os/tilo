//! What an app may reach the API with as the person using it, besides what it owns: the
//! view lookup that mints a link to its own resource, the history of its own resources, the reference
//! actions that show another resource, and the shell's invitation into the workspace it shows. None of them grants
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

/// Whether `request` asks the session to run one tilolang line as the person who typed it
/// (`session:programs#run-line`): a composer runs the line its person wrote, as them, and its
/// calls are decided as their own. The app gains nothing by it — the line is the person's own
/// words — and what it asks for beyond that is decided for the app as any other call is.
pub(super) fn programs_request(request: &tilo_actions::remote::InvokeRequest) -> bool {
    use tilo_actions::remote::session_actions;
    request.target.provider == tilo_actions::remote::SESSION_ACTIONS_PROVIDER
        && request.target.address.resource == session_actions::PROGRAMS_RESOURCE
        && request.target.address.action == session_actions::RUN_LINE
}

/// Whether `request` asks the session's shell for its invitation into the workspace — the sheet
/// where a link is made (`show-invite`) and the link itself (`issue-invitation`) — so an app
/// sharing what it shows offers the same sheet and the same invitation, decided by the person's
/// part there. It reaches nothing else of the shell — the surface up, the workspace in front, the
/// agents — and grants nothing: the invitation admits whoever redeems it, as the person's own
/// would.
pub(super) fn invitation_request(request: &tilo_actions::remote::InvokeRequest) -> bool {
    use tilo_actions::remote::session_actions;
    let action = request.target.address.action.as_str();
    request.target.provider == tilo_actions::remote::SESSION_ACTIONS_PROVIDER
        && request.target.address.resource == session_actions::INVITE_RESOURCE
        && (action == session_actions::ISSUE_INVITATION || action == session_actions::SHOW_INVITE)
}

/// Whether `request` asks the session's shell to show how an agent or a computer connects as the
/// person (`show-connect`), or for the link another of their own devices joins with
/// (`show-pair-device`): what their own card opens, shown to them on their own screen. An app
/// listing what acts as the person offers adding to it this way; it grants nothing, and the person
/// still signs anything in themselves.
pub(super) fn connect_request(request: &tilo_actions::remote::InvokeRequest) -> bool {
    use tilo_actions::remote::session_actions;
    let action = request.target.address.action.as_str();
    request.target.provider == tilo_actions::remote::SESSION_ACTIONS_PROVIDER
        && request.target.address.resource == session_actions::INVITE_RESOURCE
        && (action == session_actions::SHOW_CONNECT || action == session_actions::SHOW_PAIR_DEVICE)
}

/// Whether `request` reads the workspace's history of `app_id`'s own resources alone: what the
/// app's own History lists, as the person using it reads it there. Any other read of the
/// workspace's activity, another app's resources among it, is decided for the app.
pub(super) fn own_history_request(
    app_id: &str,
    request: &tilo_actions::remote::InvokeRequest,
) -> bool {
    use tilo_actions::remote::{InvokeArgument, workspace_activity as wire};
    let InvokeArgument::Json(arguments) = &request.argument else {
        return false;
    };
    request.target == wire::target(wire::FEED)
        && serde_json::from_value::<wire::FeedQuery>(serde_json::Value::Object(arguments.clone()))
            .is_ok_and(|query| query.reads_only_resources_of(app_id))
}

#[cfg(test)]
mod own_view_lookup_tests {
    use super::own_view_lookup;
    use tilo_actions::remote::{CatalogueQuery, ViewResourceQuery};

    tilo_test_env::scenario! {
        fn an_app_looks_up_views_only_within_its_admission;

        phase an_app_may_look_up_only_its_own_resources_views;
        {
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

        phase only_linking_and_opening_a_resource_are_open_to_every_app;
        {
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
                signed: None,
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
}

#[cfg(test)]
mod invitation_request_tests {
    use crate::apps::collab_app_broker::AppBrokerState;
    use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use crate::workspaces::providers::{Providers, Reach};
    use std::sync::{Arc, Mutex};
    use tilo_actions::InvocationId;
    use tilo_actions::remote::admitted::{offered, top_level};
    use tilo_actions::remote::session_actions;
    use tilo_actions::remote::{
        ActionCallError, ActionProvider, Admitted, Caller, CatalogueQuery, InvokeRequest, Outcome,
        Pending, RemoteActionMessage, pending,
    };

    struct Shell {
        started: Mutex<Vec<String>>,
    }

    impl tilo_actions::remote::Admits for Shell {
        const PROVIDER: &'static str = session_actions::PROVIDER;
        const APP: &'static str = session_actions::PROVIDER;

        fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
            top_level(self.descriptors())
        }

        fn start(
            &self,
            _caller: &Caller,
            request: &InvokeRequest,
            _admitted: &Admitted,
        ) -> Result<Pending<Outcome>, String> {
            self.started
                .lock()
                .unwrap()
                .push(request.target.address.action.clone());
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

    impl Shell {
        fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
            vec![tilo_actions::ActionDescriptor {
                did: None,
                state: tilo_actions::ActionState::default(),
                semantic_identity: None,
                address: tilo_actions::ActionAddress::new(
                    session_actions::INVITE_RESOURCE,
                    session_actions::ISSUE_INVITATION,
                ),
                resource_kind: "session".into(),
                resource_label: "Invite".into(),
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
    }

    /// `shell` registered on this device as the session's provider.
    fn providers(shell: Arc<Shell>) -> Providers {
        let providers = Providers::default();
        providers.provide(
            session_actions::PROVIDER,
            session_actions::PROVIDER,
            Reach::ThisDevice,
            shell as Arc<dyn ActionProvider + Send + Sync>,
        );
        providers
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
            signed: None,
        }
    }

    fn invitation() -> tilo_actions::remote::InvokeRequest {
        request(
            session_actions::PROVIDER,
            session_actions::INVITE_RESOURCE,
            session_actions::ISSUE_INVITATION,
        )
    }

    tilo_test_env::scenario! {
        fn an_apps_invitation_request_reaches_only_the_shell;

        phase an_app_asks_the_shell_for_the_invitation_and_nothing_else;
        {
            let shell = |action: &str| {
                request(
                    tilo_actions::remote::SESSION_ACTIONS_PROVIDER,
                    session_actions::INVITE_RESOURCE,
                    action,
                )
            };
            assert!(super::invitation_request(&shell(
                session_actions::ISSUE_INVITATION
            )));
            assert!(
                super::invitation_request(&shell(session_actions::SHOW_INVITE)),
                "the sheet an app shares through is the person's own invite sheet"
            );
            assert!(
                super::connect_request(&shell(session_actions::SHOW_CONNECT)),
                "an app offers the person's own connect setup"
            );
            assert!(
                super::connect_request(&shell(session_actions::SHOW_PAIR_DEVICE)),
                "an app offers the person's own code for another of their devices"
            );
            for other in [
                "invitations",
                "cancel-invitation",
                "switch-workspace",
                "show-surface",
            ] {
                assert!(
                    !super::invitation_request(&shell(other)) && !super::connect_request(&shell(other)),
                    "the shell's `{other}` is the person's own"
                );
            }
            assert!(
                !super::invitation_request(&request(
                    "record:so.tilo.notes:",
                    session_actions::INVITE_RESOURCE,
                    session_actions::ISSUE_INVITATION,
                )),
                "an invitation is the shell's, not a resource's"
            );
            assert!(
                !super::invitation_request(&request(
                    tilo_actions::remote::SESSION_ACTIONS_PROVIDER,
                    "session:chrome",
                    session_actions::ISSUE_INVITATION,
                )),
                "and it is the shell's resource that offers it"
            );
        }

        phase an_apps_invitation_is_refused_for_a_member_who_may_only_read;
        // An app sharing what it shows asks the shell for the invitation as the person using it: the
        // workspace's own grants decide, so a member who may only read is refused, and the shell is
        // never asked to make one.
        {
            let owner = hex::encode([7; 32]);
            let viewer = hex::encode([8; 32]);
            let mut workspace = Workspace::new(WorkspaceId::from_u128(96), "Shared folder");
            workspace.set_owner(owner.clone());
            workspace.admit(viewer.clone(), tilo_action_policy::Role::Viewer);
            let workspace_id = workspace.id().to_string();
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let shell = Arc::new(Shell {
                started: Mutex::default(),
            });
            let (state, _bridge) = AppBrokerState::test_state(
                Some(tree.shared().authority()),
                crate::apps::services::ProcessServices {
                    providers: Some(providers(shell.clone())),
                    ..Default::default()
                },
            );
            let app = "so.tilo.file-explorer";
            let ask = invitation();
            let api = |person: &Caller| {
                state
                    .app_api(person, app, &RemoteActionMessage::Invoke(ask.clone()))
                    .expect("the app reaches the shell's invitation as the person")
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
                invoke(&api(&reading)),
                Err(ActionCallError::NotPermitted),
                "a viewer makes no invitation"
            );
            assert!(shell.started.lock().unwrap().is_empty());

            let owning = Caller::local_person(owner).in_workspace(workspace_id);
            invoke(&api(&owning)).expect("the owner makes one");
            assert_eq!(
                *shell.started.lock().unwrap(),
                [session_actions::ISSUE_INVITATION]
            );
        }
    }
}

#[cfg(test)]
mod own_history_request_tests {
    use crate::apps::collab_app_broker::AppBrokerState;
    use crate::apps::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use tilo_actions::remote::{
        Caller, InvokeArgument, InvokeRequest, RemoteActionMessage, workspace_activity as wire,
    };

    fn feed(query: &wire::FeedQuery) -> RemoteActionMessage {
        read(wire::FEED, wire::argument(query))
    }

    fn read(action: &str, argument: InvokeArgument) -> RemoteActionMessage {
        RemoteActionMessage::Invoke(InvokeRequest {
            on_behalf_of: None,
            invocation: tilo_actions::InvocationId::new(),
            target: wire::target(action),
            argument,
            context: None,
            confirmation: None,
            revision: None,
            wait_ms: None,
            appearance: None,
            signed: None,
        })
    }

    fn of(app: &str) -> wire::FeedQuery {
        wire::FeedQuery {
            resources: Some(wire::WindowResources {
                app: app.into(),
                resources: vec!["@tilo:package-primary".into()],
                item: None,
            }),
            ..wire::FeedQuery::default()
        }
    }

    tilo_test_env::scenario! {
        fn an_app_reads_the_history_of_its_own_resources_alone;

        phase an_app_reads_its_own_resources_as_its_person_and_another_apps_not;
        // Notes reads what changed in its own resources as the person using it, without declaring
        // `[actions] invoke`. Another app asking for Notes' resources, Notes asking for the whole
        // workspace's history or every workspace's, is decided for the app itself, which holds no
        // grant: refused.
        {
            let owner = hex::encode([7; 32]);
            let mut workspace = Workspace::new(WorkspaceId::from_u128(97), "Notes");
            workspace.set_owner(owner.clone());
            let person = &Caller::local_person(owner).in_workspace(workspace.id().to_string());
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let (state, _bridge) = AppBrokerState::test_state(
                Some(tree.shared().authority()),
                crate::apps::services::ProcessServices::default(),
            );
            assert_eq!(
                state
                    .app_api(person, "so.tilo.notes", &feed(&of("so.tilo.notes")))
                    .map(|caller| caller.acting().clone())
                    .ok(),
                Some(person.acting().clone())
            );
            assert!(
                state.app_api(person, "so.tilo.chat", &feed(&of("so.tilo.notes"))).is_err(),
                "Chat reads none of Notes' history"
            );
            assert!(
                state
                    .app_api(person, "so.tilo.notes", &feed(&wire::FeedQuery::default()))
                    .is_err(),
                "the workspace's whole history is Activity's"
            );
            let everywhere = wire::FeedQuery {
                scope: wire::Scope::AllWorkspaces,
                ..of("so.tilo.notes")
            };
            assert!(
                state.app_api(person, "so.tilo.notes", &feed(&everywhere)).is_err(),
                "an app's resources elsewhere are not the workspace it works in"
            );
            assert!(
                state
                    .app_api(person, "so.tilo.notes", &read(wire::SUMMARY, InvokeArgument::None))
                    .is_err(),
                "what is new in the workspace is Activity's"
            );
        }
    }
}
