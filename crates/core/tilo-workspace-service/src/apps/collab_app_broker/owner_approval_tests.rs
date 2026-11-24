//! An owner's workspace action is admitted on the person's approve of the call's asked
//! record, never a token a caller brings, and that approve never confers delegated ownership.
use super::super::ActionDirectoryRoot;
use super::*;
use crate::apps::services::{
    ProcessServices, WorkspaceGovernanceRequest, WorkspaceGovernanceServices,
};
use crate::apps::test_support::{Grants as _, KindSet, Workspace, WorkspaceId, WorkspaceTree};
use std::sync::Mutex;
use tilo_actions::remote::asked::AskedFacts;
use tilo_actions::remote::{InvokeArgument, RemoteActionTarget};

const RESOURCE: &str = "workspace.legacy-media";
const ACTION: &str = "import-legacy-media";

struct Governance {
    checked: bool,
    calls: Mutex<Vec<serde_json::Value>>,
}

impl WorkspaceGovernanceServices for Governance {
    fn app_id(&self) -> &'static str {
        "so.tilo.workspace"
    }
    fn handles(&self, action: &str) -> bool {
        action == ACTION
    }
    fn requires_descriptor_admission(&self, _: &str) -> bool {
        self.checked
    }
    fn descriptors(&self, _: &Workspace) -> Vec<ActionDescriptor> {
        vec![ActionDescriptor {
            did: None,
            state: tilo_actions::ActionState::default(),
            address: tilo_actions::ActionAddress::new(RESOURCE, ACTION),
            resource_kind: RESOURCE.into(),
            contract: tilo_actions::ActionContract {
                kind: tilo_action_policy::ActionKind::Outside,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                ..Default::default()
            },
            semantic_identity: None,
            resource_label: "Legacy media".into(),
            action_label: "Import".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
            effects: Vec::new(),
        }]
    }
    fn invoke(&self, request: &WorkspaceGovernanceRequest<'_>) -> Result<(), String> {
        self.calls
            .lock()
            .unwrap()
            .push(request.confirmation.clone());
        Ok(())
    }
}

fn broker(checked: bool) -> (Arc<AppBrokerState>, Arc<Governance>, Caller, InvokeRequest) {
    let (state, service, owner, request, _) = broker_with_tree(checked);
    (state, service, owner, request)
}

fn broker_with_tree(
    checked: bool,
) -> (
    Arc<AppBrokerState>,
    Arc<Governance>,
    Caller,
    InvokeRequest,
    crate::apps::test_support::WorkspaceTreeHandle,
) {
    let mut workspace = Workspace::new(WorkspaceId::from_u128(95), "Owner import");
    workspace.policy.set_owner("owner");
    workspace.allow_agent("limited", KindSet::ALL);
    let workspace_id = workspace.id().to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let service = Arc::new(Governance {
        checked,
        calls: Mutex::default(),
    });
    let (state, _) = AppBrokerState::test_state(
        Some(tree.authority()),
        ProcessServices {
            workspace_governance: Some(service.clone()),
            ..Default::default()
        },
    );
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(&workspace_id),
            address: tilo_actions::ActionAddress::new(RESOURCE, ACTION),
        },
        argument: InvokeArgument::Json(
            serde_json::from_value(serde_json::json!({"hashes": ["selected"]})).unwrap(),
        ),
        confirmation: None,
        context: None,
        revision: None,
        wait_ms: None,
        appearance: None,
        on_behalf_of: None,
        signed: None,
    };
    (
        state,
        service,
        Caller::local_person("owner").in_workspace(&workspace_id),
        request,
        tree,
    )
}

fn invoke(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<(), ActionCallError> {
    state
        .action_invoke(caller, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answered")
        .map(|_| ())
}

/// What the person is shown of an import that waits for their approve: the action as the
/// workspace offers it to them, what it is on, and what a grant given with their answer allows.
fn waits_to_import() -> ActionCallError {
    ActionCallError::asks(tilo_actions::remote::asked::AskedFacts {
        on: Some("Legacy media".into()),
        verb: "Import".into(),
        destructive: true,
        plural: Some("Legacy media".into()),
        kind: RESOURCE.into(),
        effect: Some(tilo_action_policy::ActionKind::Outside),
        ..AskedFacts::default()
    })
}

/// The person's approve of `request`, as the store that kept its asked record hands it back.
fn approval(request: &InvokeRequest) -> Confirmation {
    crate::apps::collab_app_broker::person_approved(
        tilo_actions::remote::provider_workspace(&request.target.provider).unwrap(),
        request.invocation,
    )
}

tilo_test_env::scenario! {
    fn an_owners_approve_confirms_only_its_own_call;

    phase an_owner_action_waits_for_their_approve;
    {
        let (state, service, owner, mut request) = broker(true);
        assert_eq!(
            invoke(&state, &owner, &request),
            Err(waits_to_import()),
            "nothing answered for them yet"
        );
        assert!(service.calls.lock().unwrap().is_empty());
        request.confirmation = Some(approval(&request));
        invoke(&state, &owner, &request).expect("their approve confirms it");
        assert_eq!(service.calls.lock().unwrap().len(), 1);
    }

    phase unrelated_changes_preserve_an_owners_approve;
    {
        let (state, service, owner, mut request) = broker(true);
        request.confirmation = Some(approval(&request));
        state.surfaces_changed();
        state.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
            peer_id: 123,
            app_id: "org.example.unrelated".into(),
            cluster_id: uuid::Uuid::new_v4().to_string(),
            document_id: "main".into(),
        }]);
        invoke(&state, &owner, &request).expect("unrelated discovery does not revoke consent");
        assert_eq!(service.calls.lock().unwrap().len(), 1);
    }

    phase an_owner_action_runs_an_approved_call_once;
    {
        let (state, service, owner, mut request) = broker(true);
        assert_eq!(
            invoke(&state, &owner, &request),
            Err(waits_to_import())
        );
        request.confirmation = Some(approval(&request));
        invoke(&state, &owner, &request).unwrap();
        assert_eq!(
            *service.calls.lock().unwrap(),
            vec![serde_json::json!({"confirm": true})]
        );
        invoke(&state, &owner, &request).expect("an exact retry returns its original receipt");
        assert_eq!(service.calls.lock().unwrap().len(), 1);
        let mut changed = request.clone();
        changed.argument = InvokeArgument::Json(
            serde_json::from_value(serde_json::json!({"hashes": ["other"]})).unwrap(),
        );
        assert_eq!(
            invoke(&state, &owner, &changed),
            Err(ActionCallError::InvocationConflict),
            "an admitted invocation cannot be retried with different arguments",
        );
        assert_eq!(service.calls.lock().unwrap().len(), 1);
        request.invocation = tilo_actions::InvocationId::new();
        request.confirmation = Some(Confirmation::Confirmed);
        invoke(&state, &owner, &request).unwrap();
        assert_eq!(service.calls.lock().unwrap().len(), 2);
    }

    phase an_owners_approve_does_not_replace_current_policy_or_ownership;
    {
        let (state, service, owner, mut request) = broker(true);
        let agent = Caller::local_agent("limited").in_workspace(owner.workspace().unwrap());
        request.confirmation = Some(approval(&request));
        assert_eq!(
            invoke(&state, &agent, &request),
            Err(ActionCallError::NotPermitted),
            "even a grant and its person's approve cannot confer direct owner ownership"
        );
        let other = Caller::local_person("other").in_workspace(owner.workspace().unwrap());
        assert!(invoke(&state, &other, &request).is_err());
        request.target.address.resource = "not-published".into();
        request.confirmation = Some(Confirmation::Confirmed);
        assert_eq!(
            invoke(&state, &owner, &request),
            Err(ActionCallError::UnknownAction)
        );
        assert!(service.calls.lock().unwrap().is_empty());
    }

    phase canonical_governance_rechecks_ask_and_deny_after_approval_issuance;
    {
        use tilo_action_policy::{Answer, Grantee, TreePath};

        for answer in [Answer::Ask, Answer::Deny] {
            let (state, service, owner, mut request, tree) = broker_with_tree(true);
            // Structural owners are unrestricted by grants. Use a direct person
            // governed by grants to exercise current policy, not owner override.
            let person = Caller::local_person("guest").in_workspace(owner.workspace().unwrap());
            let path = TreePath::root()
                .app("so.tilo.workspace")
                .any(tilo_action_policy::Level::Kind)
                .resource(RESOURCE)
                .action(ACTION);
            tree.write().workspaces[0].allow(Grantee::person("guest"), &path, KindSet::ALL);
            request.confirmation = Some(Confirmation::Confirmed);
            invoke(&state, &person, &request).unwrap();
            service.calls.lock().unwrap().clear();
            request.invocation = tilo_actions::InvocationId::new();
            let issued_before_change = approval(&request);
            tree.write().workspaces[0].answer(Grantee::person("guest"), &path, KindSet::ALL, answer);
            request.confirmation = None;
            // Canonical governance checks confirmation before current policy. The ask states
            // nothing of the action for this caller: no catalogue row offers the import to a
            // caller who reaches the workspace by grant rather than as a member of it, so there
            // is nothing to describe. It stated the declaration on 1b212a068fe's tree.
            assert_eq!(
                invoke(&state, &person, &request),
                Err(ActionCallError::confirmation_required())
            );
            assert!(service.calls.lock().unwrap().is_empty());

            // An earlier approval can answer Ask, but cannot override a new Deny.
            for confirmation in [issued_before_change, Confirmation::Confirmed] {
                if matches!(confirmation, Confirmation::Confirmed) {
                    request.invocation = tilo_actions::InvocationId::new();
                }
                request.confirmation = Some(confirmation);
                if answer == Answer::Deny {
                    assert_eq!(
                        invoke(&state, &person, &request),
                        Err(ActionCallError::NotPermitted)
                    );
                    assert!(service.calls.lock().unwrap().is_empty());
                } else {
                    invoke(&state, &person, &request).unwrap();
                }
            }
            if answer == Answer::Ask {
                assert_eq!(
                    *service.calls.lock().unwrap(),
                    vec![serde_json::json!({"confirm": true}); 2]
                );
            }
        }
    }
}

tilo_test_env::scenario! {
    fn legacy_governance_keeps_service_owned_confirmation;

    phase legacy_governance_retains_service_owned_admission;
    {
        let (state, service, owner, mut request) = broker(false);
        request.target.address.resource = "legacy-service-validates-this".into();
        request.confirmation = Some(approval(&request));
        invoke(&state, &owner, &request).unwrap();
        assert_eq!(*service.calls.lock().unwrap(), vec![serde_json::json!({})]);
    }

    phase legacy_governance_preserves_service_confirmation_phrases;
    {
        let (state, service, owner, mut request) = broker(false);
        request.confirmation = Some(Confirmation::Phrase("import the selected media".into()));
        invoke(&state, &owner, &request).unwrap();
        assert_eq!(
            *service.calls.lock().unwrap(),
            vec![serde_json::json!({"confirmation_phrase": "import the selected media"})]
        );
    }
}
