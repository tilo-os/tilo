//! What a broker does when two devices of one person are paired.

use super::super::AppBrokerState;
use super::RowScope;
use crate::apps::services::{HostStepOutcome, HostStepRequest, HostStepServices};
use std::sync::{Arc, Mutex};
use tilo_actions::remote::{
    Caller, Completer, Confirmation, InvokeArgument, InvokeRequest, Pending, RemoteActionMessage,
    device_pairing, pending,
};

/// The host's pairing, in its place: it records each step it was asked
/// to start and answers when the test completes it.
#[derive(Default)]
struct Host {
    started: Mutex<Vec<(String, tilo_actions::MemberId, String)>>,
    answers: Mutex<Vec<Completer<HostStepOutcome>>>,
}

impl HostStepServices for Host {
    fn provider(&self) -> &'static str {
        device_pairing::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        "so.tilo.settings"
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        [device_pairing::DISCOVER, device_pairing::APPROVE]
            .into_iter()
            .map(|step| {
                let mut contract = tilo_actions::ActionContract {
                    kind: tilo_actions::ActionKind::Outside,
                    ..tilo_actions::ActionContract::default()
                };
                if step == device_pairing::APPROVE {
                    contract.risk = tilo_actions::RiskLevel::Privileged;
                    contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
                }
                tilo_actions::ActionDescriptor {
                    did: None,
                    state: tilo_actions::ActionState::default(),
                    semantic_identity: None,
                    address: tilo_actions::ActionAddress::new(device_pairing::RESOURCE, step),
                    resource_kind: device_pairing::RESOURCE.into(),
                    resource_label: "Nearby device pairing".into(),
                    action_label: step.into(),
                    keywords: Vec::new(),
                    icon: None,
                    description: None,
                    locus: tilo_actions::ExecutionLocus::default(),
                    kind: tilo_actions::OperationKind::Effect,
                    contract,
                    input: tilo_actions::InputSpec::None,
                    params: Vec::new(),
                    view: None,
                }
            })
            .collect()
    }

    fn start(&self, request: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        self.started.lock().unwrap().push((
            request.workspace.to_owned(),
            request.actor.member().clone(),
            request.address.action.clone(),
        ));
        let (answer, pending) = pending();
        self.answers.lock().unwrap().push(answer);
        Ok(pending)
    }
}

fn request(step: &str, confirmation: Option<Confirmation>) -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: device_pairing::target(step),
        argument: InvokeArgument::None,
        context: None,
        confirmation,
        revision: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

fn invoked(
    state: &Arc<AppBrokerState>,
    caller: &Caller,
    request: &InvokeRequest,
) -> Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionCallError> {
    state
        .action_invoke(caller, None, None, request)
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .expect("admission answers at once")
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the test keeps its fixture and guards alive through its assertions"
)]
#[test]
fn settings_pairs_as_its_person_and_the_outcome_follows_the_host() {
    let host = Arc::new(Host::default());
    let fixture = super::super::host_steps::signed_test_support::Fixture::new(
        crate::apps::services::ProcessServices {
            host_steps: vec![host.clone()],
            ..Default::default()
        },
    );
    let state = fixture.state.clone();
    let person = fixture.caller.clone();
    let workspace = fixture.workspace.clone();
    let owner = person
        .acting()
        .person_id()
        .expect("the settings caller is a person")
        .to_owned();

    let rows = state
        .eager_catalogue(&person, RowScope::Provider(device_pairing::PROVIDER))
        .0;
    assert_eq!(rows.len(), 2, "both steps are offered to the person here");
    assert!(rows.iter().all(|row| row.app_id == "so.tilo.settings"));

    // Settings reaches the steps as the person using it, without
    // declaring `[actions] invoke`; another app does not.
    let discover = RemoteActionMessage::Invoke(request(device_pairing::DISCOVER, None));
    assert_eq!(
        state
            .app_api(&person, "so.tilo.settings", &discover)
            .map(|caller| caller.acting().clone())
            .ok(),
        Some(person.acting().clone())
    );
    assert!(state.app_api(&person, "so.tilo.notes", &discover).is_err());

    let unconfirmed = fixture.invoke(&request(device_pairing::APPROVE, None));
    assert_eq!(
        unconfirmed,
        Err(tilo_actions::remote::ActionCallError::confirmation_required()),
        "approval needs the person's confirmation"
    );
    assert!(host.started.lock().unwrap().is_empty(), "nothing started");

    let approved = request(device_pairing::APPROVE, Some(Confirmation::Confirmed));
    let receipt = fixture.invoke(&approved).expect("admitted");
    fixture.assert_signed(&approved, false);
    assert_eq!(
        *host.started.lock().unwrap(),
        [(
            workspace.clone(),
            tilo_actions::MemberId::person(owner.clone()),
            device_pairing::APPROVE.to_owned()
        )]
    );
    let job = state
        .remote_job(&person, &receipt.invocation)
        .expect("the invocation has a job");
    assert!(matches!(
        state.invocation_status(job),
        Some(tilo_actions::InvocationUpdate::Accepted { .. })
    ));
    let answer = host.answers.lock().unwrap().pop().expect("started");
    answer.complete(Ok(tilo_actions::ActionOutput {
        value: Some(serde_json::json!({"device": "aa"})),
        resources: Vec::new(),
    }));
    assert!(matches!(
        state.invocation_status(job),
        Some(tilo_actions::InvocationUpdate::Succeeded { .. })
    ));

    fixture.assert_signed(&approved, true);

    // A remote device, or a caller confined to no workspace, is never offered
    // pairing on this machine.
    let super::super::InvocationCaller::Local { admitted, .. } = &fixture.origin else {
        panic!("the settings fixture retains local ownership")
    };
    let certificate = admitted.certificate.as_ref().unwrap();
    let remote = Caller::authenticated(certificate.user_public_key, certificate.device_public_key)
        .in_workspace(workspace);
    assert!(
        state
            .eager_catalogue(&remote, RowScope::Provider(device_pairing::PROVIDER))
            .0
            .is_empty()
    );
    assert!(invoked(&state, &remote, &request(device_pairing::DISCOVER, None)).is_err());
    let unconfined = Caller::local_person(owner);
    assert!(
        invoked(
            &state,
            &unconfined,
            &request(device_pairing::DISCOVER, None)
        )
        .is_err()
    );
    assert_eq!(host.started.lock().unwrap().len(), 1);
}
