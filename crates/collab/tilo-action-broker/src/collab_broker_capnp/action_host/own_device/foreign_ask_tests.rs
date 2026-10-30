//! A person at a device that is not theirs is asked on one of their own.
//!
//! The host admits the person — their root and their device are proven by
//! the certificate the foreign device presented — but asking them there asks
//! nobody: the question travels to the person's own devices, and only an
//! answer to that question can witness this one invocation.

use crate::collab_broker_capnp::CapnpBrokerState;
use crate::collab_broker_capnp::host_steps::signed_test_support::Fixture;
use crate::services::{HostServices, OwnDeviceActions, OwnDeviceOutcome};
use crate::services::{HostStepOutcome, HostStepRequest, HostStepServices};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_actions::remote::identity_admin;
use tilo_actions::remote::{
    ActionHostError, Caller, Completer, Confirmation, ConfirmationQuestion, InvokeArgument,
    InvokeRequest, Pending, pending,
};

/// The host's network, standing in: it records who was asked what, and
/// answers on the person's own device's behalf.
#[derive(Default)]
struct OwnDevices {
    asked: Mutex<Vec<(Caller, ConfirmationQuestion)>>,
    answers: Mutex<Vec<Completer<Result<Confirmation, ActionHostError>>>>,
    unreachable: Option<ActionHostError>,
}

impl OwnDevices {
    fn unreachable(error: ActionHostError) -> Self {
        Self {
            unreachable: Some(error),
            ..Self::default()
        }
    }

    /// What the person answered on their own device, once the host has asked
    /// them: the question is put from another thread.
    fn answer(&self, answer: Result<Confirmation, ActionHostError>) {
        let deadline = Instant::now()
            .checked_add(Duration::from_secs(60))
            .expect("a minute from now is a representable instant");
        loop {
            let waiting = self.answers.lock().unwrap().pop();
            if let Some(completer) = waiting {
                return completer.complete(answer);
            }
            assert!(Instant::now() < deadline, "the person was never asked");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn asked_about(&self) -> Vec<ConfirmationQuestion> {
        self.asked
            .lock()
            .unwrap()
            .iter()
            .map(|(_, question)| question.clone())
            .collect()
    }
}

impl OwnDeviceActions for OwnDevices {
    fn invoke(
        &self,
        _: &Caller,
        _: &tilo_actions::remote::own_device::Checked,
        _: &InvokeRequest,
    ) -> Result<Pending<OwnDeviceOutcome>, ActionHostError> {
        Err(ActionHostError::UnknownAction)
    }

    fn ask_person(
        &self,
        caller: &Caller,
        question: &ConfirmationQuestion,
    ) -> Result<Pending<Result<Confirmation, ActionHostError>>, ActionHostError> {
        if let Some(unreachable) = &self.unreachable {
            return Err(unreachable.clone());
        }
        self.asked
            .lock()
            .unwrap()
            .push((caller.clone(), question.clone()));
        let (answer, asked) = pending();
        self.answers.lock().unwrap().push(answer);
        Ok(asked)
    }
}

/// The host's identity store, standing in: it publishes the steps, which is
/// what makes a question about one of them askable at all.
#[derive(Default)]
struct Steps;

impl HostStepServices for Steps {
    fn provider(&self) -> &'static str {
        identity_admin::PROVIDER
    }

    fn app_id(&self) -> &'static str {
        "so.tilo.settings"
    }

    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            state: tilo_actions::ActionState::default(),
            semantic_identity: None,
            icon: None,
            address: tilo_actions::ActionAddress::new(
                identity_admin::RESOURCE,
                identity_admin::REMOVE,
            ),
            resource_kind: identity_admin::RESOURCE.into(),
            resource_label: "Identities".into(),
            action_label: "Remove".into(),
            keywords: Vec::new(),
            description: None,
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::Effect,
            contract: tilo_actions::ActionContract {
                kind: tilo_actions::ActionKind::Outside,
                risk: tilo_actions::RiskLevel::Privileged,
                confirmation: tilo_actions::ConfirmationPolicy::Confirm,
                ..tilo_actions::ActionContract::default()
            },
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }

    fn start(&self, _: &HostStepRequest<'_>) -> Result<Pending<HostStepOutcome>, String> {
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
    }
}

/// A request for a step this host publishes, which asks the person first.
fn request() -> InvokeRequest {
    InvokeRequest {
        on_behalf_of: None,
        invocation: tilo_actions::InvocationId::new(),
        target: identity_admin::target(identity_admin::REMOVE),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        wait_ms: None,
        appearance: None,
    }
}

fn services(own_device: Option<Arc<dyn OwnDeviceActions>>) -> HostServices {
    HostServices {
        host_steps: vec![Arc::new(Steps) as Arc<dyn HostStepServices>],
        own_device,
        ..HostServices::default()
    }
}

fn asked_there(own_devices: &Arc<OwnDevices>) -> Fixture {
    Fixture::new_certified_foreign(services(Some(
        Arc::clone(own_devices) as Arc<dyn OwnDeviceActions>
    )))
}

#[test]
fn a_person_at_a_foreign_device_is_asked_on_one_of_their_own() {
    let own_devices = Arc::new(OwnDevices::default());
    let fixture = asked_there(&own_devices);
    assert!(
        CapnpBrokerState::asks_on_own_device(&fixture.caller),
        "a person at a foreign device is asked where they are"
    );
    let waiting = fixture
        .state
        .request_witness(&fixture.caller, None, &request());
    let asked = own_devices.asked_about();
    assert_eq!(asked.len(), 1, "one question names one invocation");
    assert_eq!(asked[0].room, fixture.room, "about the room it runs in");
    assert_eq!(
        (
            asked[0].action_label.as_str(),
            asked[0].resource_label.as_str()
        ),
        ("Remove", "Identities"),
        "named as this host names the action"
    );
    own_devices.answer(Ok(Confirmation::Witnessed("allowed".into())));
    let answered = waiting.wait_timeout(Duration::from_secs(30));
    assert!(
        matches!(answered, Ok(Some(Ok(tilo_actions::remote::ConfirmedInvocation { confirmation: Confirmation::Witnessed(ref token), arguments: None }))) if token != "allowed"),
        "the host answers with its own witness, not the device's word"
    );
    drop(fixture);
}

#[test]
fn a_denied_answer_confirms_nothing() {
    let own_devices = Arc::new(OwnDevices::default());
    let fixture = asked_there(&own_devices);
    let waiting = fixture
        .state
        .request_witness(&fixture.caller, None, &request());
    own_devices.answer(Err(ActionHostError::NotPermitted));
    assert!(matches!(
        waiting.wait_timeout(Duration::from_secs(30)),
        Ok(Some(Err(ActionHostError::ConfirmationRequired)))
    ));
    drop(fixture);
}

#[test]
fn a_device_of_their_own_is_asked_here() {
    let own_devices = Arc::new(OwnDevices::default());
    let fixture = asked_there(&own_devices);
    let home = fixture.caller.clone().from_foreign_device(false);
    assert!(
        !CapnpBrokerState::asks_on_own_device(&home),
        "at a device of their own the question stays here"
    );
    drop(fixture);
}

#[test]
fn without_an_own_device_nobody_is_asked() {
    let fixture = Fixture::new_certified_foreign(services(None));
    assert!(matches!(
        fixture
            .state
            .request_witness(&fixture.caller, None, &request())
            .wait_timeout(Duration::from_secs(5)),
        Ok(Some(Err(ActionHostError::ConfirmationRequired)))
    ));
    drop(fixture);
}

#[test]
fn an_unanswered_question_refuses_the_invocation() {
    let own_devices = Arc::new(OwnDevices::default());
    let fixture = asked_there(&own_devices);
    let request = request();
    let binding = fixture
        .state
        .witness_binding("person", &fixture.room, &request);
    // Nobody answers on the person's own device: what comes back to the
    // foreign device is a refusal, never a confirmation.
    let answered = fixture
        .state
        .own_device_witness_within(
            &fixture.caller,
            &binding,
            &request,
            Duration::from_millis(50),
        )
        .wait_timeout(Duration::from_secs(30));
    assert!(
        matches!(
            answered,
            Ok(Some(Err(ActionHostError::ConfirmationRequired)))
        ),
        "a question nobody answers confirms nothing"
    );
    assert_eq!(own_devices.asked_about().len(), 1, "one question was asked");
    drop(fixture);
}

#[test]
fn an_unreachable_own_device_confirms_nothing() {
    let fixture = Fixture::new_certified_foreign(HostServices {
        own_device: Some(
            Arc::new(OwnDevices::unreachable(ActionHostError::Unavailable {
                reason: "no device of theirs is reachable in this workspace".into(),
            })) as Arc<dyn OwnDeviceActions>,
        ),
        ..HostServices::default()
    });
    assert!(matches!(
        fixture
            .state
            .request_witness(&fixture.caller, None, &request())
            .wait_timeout(Duration::from_secs(5)),
        Ok(Some(Err(ActionHostError::Unavailable { .. })))
    ));
    drop(fixture);
}
