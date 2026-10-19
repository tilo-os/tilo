//! Owner governance admission consumes an exact existing confirmation, not a
//! replacement approval, and never turns that approval into delegated custody.
use super::*;
use crate::services::{HostServices, WorkspaceGovernanceRequest, WorkspaceGovernanceServices};
use crate::test_support::{Grants as _, KindSet, Workspace, WorkspaceId, WorkspaceTree};
use std::sync::Mutex;
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
            description: None,
            locus: tilo_actions::ExecutionLocus::ClientLocal,
            kind: tilo_actions::OperationKind::Effect,
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
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

fn broker(
    checked: bool,
) -> (
    Arc<CapnpBrokerState>,
    Arc<Governance>,
    Caller,
    InvokeRequest,
) {
    let (state, service, owner, request, _) = broker_with_tree(checked);
    (state, service, owner, request)
}

fn broker_with_tree(
    checked: bool,
) -> (
    Arc<CapnpBrokerState>,
    Arc<Governance>,
    Caller,
    InvokeRequest,
    crate::test_support::WorkspaceTreeHandle,
) {
    let mut workspace = Workspace::new(WorkspaceId(95), "Owner import");
    workspace.set_owner("owner");
    workspace.admit_agent("limited", KindSet::ALL);
    let room = workspace.room.to_string();
    let mut tree = WorkspaceTree::default();
    tree.workspaces.push(workspace);
    let tree = tree.shared();
    let service = Arc::new(Governance {
        checked,
        calls: Mutex::default(),
    });
    let (state, _) = CapnpBrokerState::new_configured(
        &Arc::new(super::super::FanoutState::new()),
        Arc::new(Mutex::new(super::super::BrokerChannels::default())),
        super::super::PeerPublisher::disabled(),
        Some(tree.authority()),
        None,
        None,
        HostServices {
            workspace_governance: Some(service.clone()),
            ..Default::default()
        },
    );
    let request = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: tilo_actions::remote::workspace_provider(&room),
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
    };
    (
        state,
        service,
        Caller::local_person("owner").in_room(room),
        request,
        tree,
    )
}

fn invoke(
    state: &Arc<CapnpBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<(), ActionHostError> {
    state
        .action_invoke(caller, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answered")
        .map(|_| ())
}

fn witness(state: &CapnpBrokerState, actor: &str, request: &InvokeRequest) -> Confirmation {
    Confirmation::Witnessed(
        state
            .witness_confirmation(&state.witness_binding(
                actor,
                tilo_actions::remote::workspace_provider_room(&request.target.provider).unwrap(),
                request,
            ))
            .unwrap(),
    )
}

#[test]
fn owner_governance_rejects_witness_from_other_original_contexts() {
    for changed in ["provider", "room", "revision", "invocation", "publication"] {
        let (state, service, owner, mut request) = broker(true);
        let mut binding = state.witness_binding("owner", owner.room().unwrap(), &request);
        match changed {
            "provider" => binding.provider = "another-provider".into(),
            "room" => binding.room = "another-workspace".into(),
            "revision" => binding.revision = Some("previous-publication".into()),
            "invocation" => binding.invocation = tilo_actions::InvocationId::new(),
            "publication" => binding.publication.0 = binding.publication.0.wrapping_add(1),
            _ => panic!("not exercised by this test"),
        }
        request.confirmation = Some(Confirmation::Witnessed(
            state.witness_confirmation(&binding).unwrap(),
        ));
        assert_eq!(
            invoke(&state, &owner, &request),
            Err(ActionHostError::ConfirmationRequired),
            "a witness for another {changed} never confirms this invocation",
        );
        assert!(service.calls.lock().unwrap().is_empty());
        request.confirmation = Some(witness(&state, "owner", &request));
        invoke(&state, &owner, &request).expect("the exact answer remains usable");
        assert_eq!(service.calls.lock().unwrap().len(), 1);
    }
}

#[test]
fn unrelated_publication_changes_preserve_host_governance_witness() {
    let (state, service, owner, mut request) = broker(true);
    request.confirmation = Some(witness(&state, "owner", &request));
    state.surfaces_changed();
    state.replace_action_directory_roots_for_test(vec![ActionDirectoryRoot {
        peer_id: 123,
        app_id: "org.example.unrelated".into(),
        cluster_id: uuid::Uuid::new_v4().to_string(),
        record_id: "main".into(),
    }]);
    invoke(&state, &owner, &request).expect("unrelated discovery does not revoke consent");
    assert_eq!(service.calls.lock().unwrap().len(), 1);
}

#[test]
fn owner_governance_consumes_exact_witness_once() {
    let (state, service, owner, mut request) = broker(true);
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    request.confirmation = Some(witness(&state, "owner", &request));
    let mut changed = request.clone();
    changed.argument = InvokeArgument::Json(
        serde_json::from_value(serde_json::json!({"hashes": ["other"]})).unwrap(),
    );
    assert_eq!(
        invoke(&state, &owner, &changed),
        Err(ActionHostError::ConfirmationRequired)
    );
    invoke(&state, &owner, &request).unwrap();
    assert_eq!(
        *service.calls.lock().unwrap(),
        vec![serde_json::json!({"confirm": true})]
    );
    invoke(&state, &owner, &request).expect("an exact retry returns its original receipt");
    assert_eq!(service.calls.lock().unwrap().len(), 1);
    assert_eq!(
        invoke(&state, &owner, &changed),
        Err(ActionHostError::InvocationConflict),
        "an admitted invocation cannot be retried with different arguments",
    );
    assert_eq!(service.calls.lock().unwrap().len(), 1);
    request.invocation = tilo_actions::InvocationId::new();
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    request.confirmation = Some(Confirmation::Confirmed);
    invoke(&state, &owner, &request).unwrap();
    assert_eq!(service.calls.lock().unwrap().len(), 2);
}

#[test]
fn owner_governance_witness_does_not_replace_current_policy_or_custody() {
    let (state, service, owner, mut request) = broker(true);
    request.confirmation = Some(witness(&state, "someone-else", &request));
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    let mut other_address = request.clone();
    other_address.target.address.resource = "another-resource".into();
    request.confirmation = Some(witness(&state, "owner", &other_address));
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    other_address = request.clone();
    other_address.target.address.action = "different-action".into();
    request.confirmation = Some(witness(&state, "owner", &other_address));
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    let agent = Caller::local_agent("limited").in_room(owner.room().unwrap());
    request.confirmation = Some(witness(&state, "agent:limited", &request));
    assert_eq!(
        invoke(&state, &agent, &request),
        Err(ActionHostError::NotPermitted),
        "even a grant and exact witness cannot confer direct owner custody"
    );
    let other = Caller::local_person("other").in_room(owner.room().unwrap());
    request.confirmation = Some(witness(&state, "other", &request));
    assert!(invoke(&state, &other, &request).is_err());
    request.target.address.resource = "not-published".into();
    request.confirmation = Some(Confirmation::Confirmed);
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::UnknownAction)
    );
    assert!(service.calls.lock().unwrap().is_empty());
}

#[test]
fn canonical_governance_rechecks_ask_and_deny_after_witness_issuance() {
    use tilo_action_policy::{Answer, Subject, TreePath};

    for answer in [Answer::Ask, Answer::Deny] {
        let (state, service, owner, mut request, tree) = broker_with_tree(true);
        // Structural owners are unrestricted by grants. Use a direct person
        // governed by grants to exercise current policy, not owner override.
        let person = Caller::local_person("guest").in_room(owner.room().unwrap());
        let path = TreePath::root()
            .app("so.tilo.workspace")
            .any(tilo_action_policy::Level::Kind)
            .resource(RESOURCE)
            .action(ACTION);
        tree.write().workspaces[0].allow(Subject::person("guest"), &path, KindSet::ALL);
        request.confirmation = Some(Confirmation::Confirmed);
        invoke(&state, &person, &request).unwrap();
        service.calls.lock().unwrap().clear();
        request.invocation = tilo_actions::InvocationId::new();
        let issued_before_change = witness(&state, "guest", &request);
        tree.write().workspaces[0].answer(Subject::person("guest"), &path, KindSet::ALL, answer);
        request.confirmation = None;
        // Canonical governance checks confirmation before current policy.
        assert_eq!(
            invoke(&state, &person, &request),
            Err(ActionHostError::ConfirmationRequired)
        );
        assert!(service.calls.lock().unwrap().is_empty());

        // An earlier witness can answer Ask, but cannot override a new Deny.
        for confirmation in [issued_before_change, Confirmation::Confirmed] {
            if matches!(confirmation, Confirmation::Confirmed) {
                request.invocation = tilo_actions::InvocationId::new();
            }
            request.confirmation = Some(confirmation);
            if answer == Answer::Deny {
                assert_eq!(
                    invoke(&state, &person, &request),
                    Err(ActionHostError::NotPermitted)
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

#[test]
fn expired_owner_witness_cannot_confirm_governance() {
    let (state, service, owner, mut request) = broker(true);
    let confirmation = witness(&state, "owner", &request);
    let Confirmation::Witnessed(token) = &confirmation else {
        panic!("not exercised by this test")
    };
    state
        .witnessed_confirmations
        .lock()
        .unwrap()
        .get_mut(token)
        .unwrap()
        .expires = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(1))
        .unwrap();
    request.confirmation = Some(confirmation);
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    request.confirmation = Some(Confirmation::Witnessed("unregistered".into()));
    assert_eq!(
        invoke(&state, &owner, &request),
        Err(ActionHostError::ConfirmationRequired)
    );
    assert!(service.calls.lock().unwrap().is_empty());
}

#[test]
fn legacy_governance_retains_service_owned_admission() {
    let (state, service, owner, mut request) = broker(false);
    request.target.address.resource = "legacy-service-validates-this".into();
    request.confirmation = Some(witness(&state, "owner", &request));
    let Some(Confirmation::Witnessed(token)) = request.confirmation.as_ref() else {
        panic!("not exercised by this test")
    };
    invoke(&state, &owner, &request).unwrap();
    assert_eq!(*service.calls.lock().unwrap(), vec![serde_json::json!({})]);
    assert!(
        state
            .witnessed_confirmations
            .lock()
            .unwrap()
            .contains_key(token),
        "service-owned admission does not consume a broker owner witness"
    );
}

#[test]
fn legacy_governance_preserves_service_confirmation_phrases() {
    let (state, service, owner, mut request) = broker(false);
    request.confirmation = Some(Confirmation::Phrase("import the selected media".into()));
    invoke(&state, &owner, &request).unwrap();
    assert_eq!(
        *service.calls.lock().unwrap(),
        vec![serde_json::json!({"confirmation_phrase": "import the selected media"})]
    );
}
