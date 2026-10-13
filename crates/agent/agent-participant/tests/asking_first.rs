//! An agent never confirms by its own word. Over the real session, a task
//! step whose action asks a person first is put to the person through their
//! own host (`ActionHost::confirm`) while the agent shows it is waiting; it
//! runs only on the person's witnessed answer, and is refused when nobody
//! is there to ask or the person says no.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
    clippy::arithmetic_side_effects,
    clippy::too_many_lines,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard; one scenario in order, \
              driven on the test's own thread (the agent's apps are not Send)"
)]

mod common;

tilo_agent_participant::app_worker!();
tilo_automerge_isolate::test_worker!();
#[path = "../../../browser/tilo-browser-peer/tests/support/steward.rs"]
mod steward;
#[path = "../../../browser/tilo-browser-peer/tests/support/mod.rs"]
mod support;

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use common::{KeywordModel, catalogue};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueQuery,
    Confirmation, InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};
use tilo_agent_participant::{
    AgentControl, AgentIdentity, AgentParticipant, ModelPlanner, ParticipantConfig,
};
use tilo_relay_protocol::agents::{AgentStatus, TaskState};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use support::{ROOM, enrol, start_host, wait_for};

const CALCULATOR: &str = "so.tilo.calculator";

/// The person's host: it records each question and answers with what the
/// test hands it, when the test hands it.
#[derive(Default)]
struct PersonHost {
    asked: Mutex<Vec<(Caller, InvokeRequest)>>,
    answers: Mutex<Vec<tilo_actions::remote::Completer<Result<Confirmation, ActionHostError>>>>,
}

impl ActionHost for PersonHost {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        Pending::ready(Err(ActionHostError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Result<(), ActionHostError> {
        Err(ActionHostError::UnknownInvocation)
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<Confirmation, ActionHostError>> {
        self.asked
            .lock()
            .unwrap()
            .push((caller.clone(), request.clone()));
        let (completer, answer) = pending();
        self.answers.lock().unwrap().push(completer);
        answer
    }
}

impl PersonHost {
    fn answer(&self, answer: Result<Confirmation, ActionHostError>) {
        let completer = self.answers.lock().unwrap().remove(0);
        completer.complete(answer);
    }
}

/// Work out one sum, then clear the calculator's history: an action that
/// asks a person first.
fn clear_history() -> String {
    serde_json::json!([
        {
            "app": CALCULATOR,
            "resource": "calculator.document",
            "action": "commit-history",
            "arguments": {"expr": "1+1", "result": "2"},
        },
        {
            "app": CALCULATOR,
            "resource": "calculator.document",
            "action": "clear-history",
        },
    ])
    .to_string()
}

fn last_task(control: &AgentControl) -> tilo_relay_protocol::agents::AgentTask {
    control.info().tasks.last().cloned().unwrap()
}

/// Pump `agent` until the last task ends.
fn until_done(agent: &mut AgentParticipant, control: &AgentControl) {
    let deadline = Instant::now() + Duration::from_secs(20);
    agent.run_until(Some(deadline), |_| {
        !matches!(
            last_task(control).state,
            TaskState::Queued | TaskState::Running
        )
    });
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_agent_asks_its_person_before_an_action_that_asks_first() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(&root, "home-server", DeviceCapabilities::host());
    let state = tempfile::tempdir().unwrap();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let agent_device = enrol(
        &root,
        "Agent: Careful",
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &root,
        agent_device.cert.device_public_key,
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: "careful".into(),
        },
        "Careful agent",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            rooms: vec![ROOM.into()],
            apps: vec![CALCULATOR.into()],
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let control = AgentControl::new(delegation.clone());
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue(),
        Box::new(ModelPlanner::new(Box::new(KeywordModel))),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    // 1. Nobody to ask: the agent does not confirm for the person.
    control.message(&clear_history()).unwrap();
    until_done(&mut agent, &control);
    let refused = last_task(&control);
    assert_eq!(refused.state, TaskState::Failed, "{refused:?}");
    assert!(
        refused
            .error
            .as_deref()
            .is_some_and(|error| error.contains("confirmation_required")),
        "{refused:?}"
    );

    // 2. The person's host is asked, the agent shows it is waiting, and the
    // step runs once the person's device witnessed it.
    let person = Arc::new(PersonHost::default());
    control.set_witness(Some(person.clone()));
    control.message(&clear_history()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    agent.run_until(Some(deadline), |_| !person.asked.lock().unwrap().is_empty());
    {
        let asked = person.asked.lock().unwrap();
        let (caller, request) = &asked[0];
        assert_eq!(
            caller.principal(),
            &CallerPrincipal::Agent("careful".into()),
            "the agent asks as itself"
        );
        assert_eq!(
            caller.user_public_key(),
            Some(&root.verifying_key().to_bytes())
        );
        assert_eq!(request.target.address.action, "clear-history");
        assert_eq!(request.confirmation, None, "it offers no word of its own");
    }
    assert!(
        control
            .activity()
            .is_some_and(|activity| activity.starts_with("Waiting for confirmation")),
        "{:?}",
        control.activity()
    );
    assert_eq!(last_task(&control).state, TaskState::Running);
    person.answer(Ok(Confirmation::Witnessed("from Ada's device".into())));
    until_done(&mut agent, &control);
    let confirmed = last_task(&control);
    assert_eq!(confirmed.state, TaskState::Completed, "{confirmed:?}");

    // 3. The person says no: nothing runs.
    control.message(&clear_history()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    agent.run_until(Some(deadline), |_| person.asked.lock().unwrap().len() == 2);
    person.answer(Err(ActionHostError::NotPermitted));
    until_done(&mut agent, &control);
    let declined = last_task(&control);
    assert_eq!(declined.state, TaskState::Failed, "{declined:?}");
    agent.close();
    host.relay.shutdown().await;
}
