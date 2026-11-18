//! An agent never confirms by its own word. Over the real session, a task
//! step whose action asks a person first is put to the person through their
//! own host (`ActionProvider::confirm`) while the agent shows it is waiting; it
//! runs only on the person's approved answer, and is refused when nobody
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

use std::sync::{Arc, Mutex};

use crate::common::{KeywordModel, catalogue};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, Confirmation,
    ConfirmedInvocation, InvocationReceipt, InvocationState, InvokeRequest, Pending, pending,
};
use tilo_agent_session::{AgentControl, AgentIdentity, AgentMember, MemberConfig, ModelPlanner};
use tilo_server_protocol::agents::{AgentStatus, TaskState};
use tilo_server_protocol::delegation::{DelegateScope, Delegation};
use tilo_user_identity::{CertificationPolicy, DeviceCapabilities, UserIdentity};

use crate::support::{WORKSPACE, enrol, start_host, wait_for};
use tilo_server_protocol::WorkspaceId;

const EBOOK: &str = "so.tilo.ebook";

/// The person's host: it records each question and answers with what the
/// test hands it, when the test hands it.
#[derive(Default)]
struct PersonHost {
    asked: Mutex<Vec<(Caller, InvokeRequest)>>,
    answers:
        Mutex<Vec<tilo_actions::remote::Completer<Result<ConfirmedInvocation, ActionCallError>>>>,
}

impl ActionProvider for PersonHost {
    fn query(
        &self,
        _: &Caller,
        _: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn invoke(
        &self,
        _: &Caller,
        _: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Err(ActionCallError::UnknownAction))
    }

    fn observe(&self, _: &Caller, _: &tilo_actions::InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(
        &self,
        _: &Caller,
        _: &tilo_actions::InvocationId,
    ) -> tilo_actions::remote::Pending<Result<(), ActionCallError>> {
        tilo_actions::remote::Pending::ready(Err(ActionCallError::UnknownInvocation))
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<ConfirmedInvocation, ActionCallError>> {
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
    fn answer(&self, answer: Result<Confirmation, ActionCallError>) {
        let completer = self.answers.lock().unwrap().remove(0);
        completer.complete(answer.map(ConfirmedInvocation::unchanged));
    }
}

/// Put a book on the shelf, then take it off again: Undo puts the row back
/// but not this device's access to the book's file, so its contract asks a
/// person first (`#[action(irreversible)]`). Clearing the calculator's
/// history no longer does: Undo takes that back, so it runs at once.
fn shelve_then_remove() -> String {
    serde_json::json!([
        {
            "app": EBOOK,
            "resource": "ebook.library",
            "action": "shelve",
            "arguments": {
                "book": "book:dusk",
                "title": "Dusk",
                "author": "Ada",
                "subject": "",
                "shared": false,
            },
        },
        {
            "app": EBOOK,
            "resource": "ebook.library",
            "action": "remove-from-library",
            "arguments": {"book": "book:dusk"},
        },
    ])
    .to_string()
}

fn last_task(control: &AgentControl) -> tilo_server_protocol::agents::AgentTask {
    control.info().tasks.last().cloned().unwrap()
}

/// Pump `agent` until the last task ends.
fn until_done(agent: &mut AgentMember, control: &AgentControl) {
    let deadline = tilo_test_env::hang_deadline();
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
    let host_device = enrol(&root, "home-server", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    crate::support::place(&host.server, &host_device.cert).await;
    let agent_device = enrol(
        &root,
        "Agent: Careful",
        CertificationPolicy::agent().ceiling,
    );
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &root,
        agent_device.cert.device_public_key,
        tilo_server_protocol::delegation::Delegate::Agent {
            agent_id: "careful".into(),
        },
        "Careful agent",
        [0x7c, 0x3a, 0xed],
        DelegateScope {
            workspaces: vec![WorkspaceId::parse(WORKSPACE).unwrap()],
            apps: vec![EBOOK.into()],
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.server
        .register_agent(&agent_device.cert, delegation.clone())
        .await
        .unwrap();
    let control = AgentControl::new(delegation.clone());
    let mut agent = AgentMember::start(
        MemberConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            workspace: WORKSPACE.into(),
            content: tilo_content::ContentStore::memory(),
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
    control.message(&shelve_then_remove()).unwrap();
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
    // step runs once the person's device approved it.
    let person = Arc::new(PersonHost::default());
    control.set_approver(Some(person.clone()));
    control.message(&shelve_then_remove()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    agent.run_until(Some(deadline), |_| !person.asked.lock().unwrap().is_empty());
    {
        let asked = person.asked.lock().unwrap();
        let (caller, request) = &asked[0];
        assert_eq!(
            caller.acting().member_id(),
            Some(&tilo_action_policy::MemberId::agent("careful")),
            "the agent asks as itself"
        );
        assert_eq!(
            caller.user_public_key(),
            Some(&root.verifying_key().to_bytes())
        );
        assert_eq!(request.target.address.action, "remove-from-library");
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
    person.answer(Ok(Confirmation::Approved("from Ada's device".into())));
    until_done(&mut agent, &control);
    let confirmed = last_task(&control);
    assert_eq!(confirmed.state, TaskState::Completed, "{confirmed:?}");

    // 3. The person says no: nothing runs.
    control.message(&shelve_then_remove()).unwrap();
    let deadline = tilo_test_env::hang_deadline();
    agent.run_until(Some(deadline), |_| person.asked.lock().unwrap().len() == 2);
    person.answer(Err(ActionCallError::NotPermitted));
    until_done(&mut agent, &control);
    let declined = last_task(&control);
    assert_eq!(declined.state, TaskState::Failed, "{declined:?}");
    agent.close();
    host.server.shutdown().await;
}
