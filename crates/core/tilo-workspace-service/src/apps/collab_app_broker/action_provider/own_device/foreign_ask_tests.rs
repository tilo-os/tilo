//! A person at a device that is not theirs is asked on one of their own.
//!
//! The host admits the person — their root and their device are proven by
//! the certificate the foreign device presented — but asking them there asks
//! nobody: the question travels to the person's own devices, and only an
//! answer to that question can approval this one invocation.

use crate::apps::collab_app_broker::AppBrokerState;
use crate::apps::collab_app_broker::provided::signed_test_support::Fixture;
use crate::apps::services::{OwnDeviceActions, OwnDeviceOutcome, ProcessServices};
use crate::workspaces::providers::{Providers, Reach};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tilo_actions::InvocationId;
use tilo_actions::remote::admitted::{offered, settled, top_level};
use tilo_actions::remote::identity_admin;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Admitted, Caller, CataloguePage, CatalogueQuery, Completer,
    Confirmation, ConfirmationQuestion, InvocationReceipt, InvocationState, InvokeArgument,
    InvokeRequest, Outcome, Pending, pending,
};

/// The host's network, in its place: it records who was asked what, and
/// answers on the person's own device's behalf.
#[derive(Default)]
struct OwnDevices {
    asked: Mutex<Vec<(Caller, ConfirmationQuestion)>>,
    answers: Mutex<Vec<Completer<Result<Confirmation, ActionCallError>>>>,
    unreachable: Option<ActionCallError>,
}

impl OwnDevices {
    fn unreachable(error: ActionCallError) -> Self {
        Self {
            unreachable: Some(error),
            ..Self::default()
        }
    }

    /// What the person answered on their own device, once the host has asked
    /// them: the question is put from another thread.
    fn answer(&self, answer: Result<Confirmation, ActionCallError>) {
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
    ) -> Result<Pending<OwnDeviceOutcome>, ActionCallError> {
        Err(ActionCallError::UnknownAction)
    }

    fn ask_person(
        &self,
        caller: &Caller,
        question: &ConfirmationQuestion,
    ) -> Result<Pending<Result<Confirmation, ActionCallError>>, ActionCallError> {
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

/// The host's identity store, in its place: it publishes the steps, which is
/// what makes a question about one of them askable at all.
#[derive(Default)]
struct Steps;

impl ActionProvider for Steps {
    fn query(
        &self,
        _caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        offered(
            identity_admin::PROVIDER,
            "so.tilo.settings",
            top_level(self.descriptors()),
            &query,
        )
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        self.run(caller, request, Admitted::default())
    }

    fn run(
        &self,
        caller: &Caller,
        request: InvokeRequest,
        admitted: Admitted,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        settled(request.invocation, self.start(caller, &request, &admitted))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownInvocation))
    }
}

impl Steps {
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            did: None,
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

    fn start(
        &self,
        _caller: &Caller,
        _request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        Ok(Pending::ready(Ok(tilo_actions::ActionOutput::default())))
    }
}

/// `steps` registered on this device as the identity store's provider.
fn providers() -> Providers {
    let providers = Providers::default();
    providers.provide(
        identity_admin::PROVIDER,
        "so.tilo.settings",
        Reach::ThisDevice,
        Arc::new(Steps) as Arc<dyn ActionProvider + Send + Sync>,
    );
    providers
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
        signed: None,
    }
}

fn services(own_device: Option<Arc<dyn OwnDeviceActions>>) -> ProcessServices {
    ProcessServices {
        providers: Some(providers()),
        own_device,
        ..ProcessServices::default()
    }
}

fn asked_there(own_devices: &Arc<OwnDevices>) -> Fixture {
    Fixture::new_certified_foreign(services(Some(
        Arc::clone(own_devices) as Arc<dyn OwnDeviceActions>
    )))
}

tilo_test_env::scenario! {
    fn asking_a_person_at_a_foreign_device_on_their_own;

    phase a_person_at_a_foreign_device_is_asked_on_one_of_their_own;
    {
        let own_devices = Arc::new(OwnDevices::default());
        let fixture = asked_there(&own_devices);
        assert!(
            AppBrokerState::asks_on_own_device(&fixture.caller),
            "a person at a foreign device is asked where they are"
        );
        let waiting = fixture
            .state
            .request_approval(&fixture.caller, None, &request());
        let asked = own_devices.asked_about();
        assert_eq!(asked.len(), 1, "one question names one invocation");
        assert_eq!(
            asked[0].workspace, fixture.workspace,
            "about the workspace it runs in"
        );
        assert_eq!(
            (
                asked[0].action_label.as_str(),
                asked[0].resource_label.as_str()
            ),
            ("Remove", "Identities"),
            "named as this host names the action"
        );
        own_devices.answer(Ok(Confirmation::Approved("allowed".into())));
        let answered = waiting.wait_timeout(Duration::from_secs(30));
        assert!(
            matches!(answered, Ok(Some(Ok(tilo_actions::remote::ConfirmedInvocation { confirmation: Confirmation::Approved(ref token), arguments: None }))) if token != "allowed"),
            "the host answers with its own approval, not the device's word"
        );
        drop(fixture);
    }

    phase a_denied_answer_confirms_nothing;
    {
        let own_devices = Arc::new(OwnDevices::default());
        let fixture = asked_there(&own_devices);
        let waiting = fixture
            .state
            .request_approval(&fixture.caller, None, &request());
        own_devices.answer(Err(ActionCallError::NotPermitted));
        assert!(matches!(
            waiting.wait_timeout(Duration::from_secs(30)),
            Ok(Some(Err(ActionCallError::ConfirmationRequired { .. })))
        ));
        drop(fixture);
    }

    phase a_device_of_their_own_is_asked_here;
    {
        let own_devices = Arc::new(OwnDevices::default());
        let fixture = asked_there(&own_devices);
        let home = fixture.caller.clone().from_foreign_device(false);
        assert!(
            !AppBrokerState::asks_on_own_device(&home),
            "at a device of their own the question stays here"
        );
        drop(fixture);
    }

    phase without_an_own_device_nobody_is_asked;
    {
        let fixture = Fixture::new_certified_foreign(services(None));
        assert!(matches!(
            fixture
                .state
                .request_approval(&fixture.caller, None, &request())
                .wait_timeout(Duration::from_secs(5)),
            Ok(Some(Err(ActionCallError::ConfirmationRequired { .. })))
        ));
        drop(fixture);
    }

    phase an_unanswered_question_refuses_the_invocation;
    {
        let own_devices = Arc::new(OwnDevices::default());
        let fixture = asked_there(&own_devices);
        let request = request();
        let binding = fixture
            .state
            .approval_binding(&tilo_actions::MemberId::person("person"), &fixture.workspace, &request);
        // Nobody answers on the person's own device: what comes back to the
        // foreign device is a refusal, never a confirmation.
        let answered = fixture
            .state
            .own_device_approval_within(
                &fixture.caller,
                &binding,
                &request,
                Duration::from_millis(50),
            )
            .wait_timeout(Duration::from_secs(30));
        assert!(
            matches!(
                answered,
                Ok(Some(Err(ActionCallError::ConfirmationRequired { .. })))
            ),
            "a question nobody answers confirms nothing"
        );
        assert_eq!(own_devices.asked_about().len(), 1, "one question was asked");
        drop(fixture);
    }

    phase an_unreachable_own_device_confirms_nothing;
    {
        let fixture = Fixture::new_certified_foreign(ProcessServices {
            own_device: Some(
                Arc::new(OwnDevices::unreachable(ActionCallError::Unavailable {
                    reason: "no device of theirs is reachable in this workspace".into(),
                })) as Arc<dyn OwnDeviceActions>,
            ),
            ..ProcessServices::default()
        });
        assert!(matches!(
            fixture
                .state
                .request_approval(&fixture.caller, None, &request())
                .wait_timeout(Duration::from_secs(5)),
            Ok(Some(Err(ActionCallError::Unavailable { .. })))
        ));
        drop(fixture);
    }
}
