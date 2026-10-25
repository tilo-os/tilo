//! A task's check compares data, not pictures. Over the real session, an
//! agent's model plan changes a video project; the check its host plans
//! once the steps ran is shown the facts the project states (the read named
//! `facts`), with the change in them. The first plan is not.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::future_not_send,
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

use common::catalogue;
use tilo_actions::door::ActionAddress;
use tilo_agent_participant::{
    AgentControl, AgentIdentity, AgentParticipant, ParticipantConfig, PlanContext, Planner, Step,
    StepAction,
};
use tilo_relay_protocol::agents::{AgentStatus, TaskState};
use tilo_relay_protocol::delegation::{DelegateScope, Delegation};
use tilo_user_identity::{CertificationPolicy, UserIdentity};

use support::{ROOM, enrol, start_host, wait_for};

const VIDEO: &str = "so.tilo.video-editor";

/// A model's planner that checks work: its first plan sets the project's
/// frame size, every later plan (the check) plans nothing. It keeps each
/// plan's task and what it was shown the records hold.
struct Recording {
    plans: Plans,
}

/// Each plan's task and the lines it was shown of what records hold.
type Plans = Arc<Mutex<Vec<(String, Vec<String>)>>>;

impl Planner for Recording {
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String> {
        let mut plans = self.plans.lock().unwrap();
        plans.push((task.to_owned(), context.snapshots.clone()));
        if plans.len() > 1 {
            return Ok(Vec::new());
        }
        let mut arguments = serde_json::Map::new();
        arguments.insert("width".into(), 1280.into());
        arguments.insert("height".into(), 720.into());
        Ok(vec![Step {
            app: VIDEO.into(),
            record: None,
            action: StepAction::Invoke {
                address: ActionAddress::new("video-editor.project", "set-frame-size"),
                arguments,
            },
            carries: None,
            serves: None,
        }])
    }

    fn checks_work(&self) -> bool {
        true
    }

    fn last_planned_by_model(&self) -> bool {
        true
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_check_is_shown_the_facts_of_the_record_its_task_changed() {
    let (root, _) = UserIdentity::generate("Ada", 1);
    let host_device = enrol(
        &root,
        "home-server",
        tilo_user_identity::DeviceCapabilities::host(),
    );
    let state = support::scratch_dir();
    let host = start_host(
        &root,
        &host_device,
        state.path(),
        "127.0.0.1:0".parse().unwrap(),
    )
    .await;
    let _steward = steward::Steward::start(&host.relay, &root).await;
    let agent_device = enrol(&root, "Agent: Ed", CertificationPolicy::agent().ceiling);
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &root,
        agent_device.cert.device_public_key,
        tilo_relay_protocol::delegation::Delegate::Agent {
            agent_id: "ed".into(),
        },
        "Ed",
        [0x2a, 0x7c, 0xd1],
        DelegateScope {
            rooms: vec![ROOM.into()],
            apps: vec![VIDEO.into()],
        },
        now,
        now + 3_600_000_000_000,
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let control = AgentControl::new(delegation.clone());
    let plans = Arc::new(Mutex::new(Vec::new()));
    let mut agent = AgentParticipant::start(
        ParticipantConfig {
            host: host.iroh(),
            host_cert: host_device.cert.clone(),
            room: ROOM.into(),
            content: tilo_content::ContentStore::memory(),
        },
        AgentIdentity {
            key: agent_device.key.clone(),
            cert: agent_device.cert.clone(),
            delegation,
        },
        control.clone(),
        catalogue(),
        Box::new(Recording {
            plans: Arc::clone(&plans),
        }),
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    control
        .message("Cut the promo at 1280x720 in the video editor")
        .unwrap();
    let deadline = tilo_test_env::hang_deadline();
    agent.run_until(Some(deadline), |_| {
        control
            .info()
            .tasks
            .last()
            .is_some_and(|task| !matches!(task.state, TaskState::Queued | TaskState::Running))
    });
    let task = control.info().tasks.last().cloned().unwrap();
    assert_eq!(task.state, TaskState::Completed, "{task:?}");

    let plans = plans.lock().unwrap().clone();
    assert_eq!(plans.len(), 2, "the plan, then its check: {plans:?}");
    let facts_of = |shown: &[String]| -> Vec<String> {
        shown
            .iter()
            .filter(|line| line.contains("video-editor.project#facts states it now"))
            .cloned()
            .collect()
    };
    assert!(
        facts_of(&plans[0].1).is_empty(),
        "a first plan is shown what records hold, not their facts: {:?}",
        plans[0].1
    );
    let (check, shown) = &plans[1];
    assert!(check.starts_with("Check what this task made"), "{check}");
    let facts = facts_of(shown);
    assert_eq!(facts.len(), 1, "{shown:?}");
    assert!(
        facts[0].contains(r#""width":1280"#) && facts[0].contains(r#""height":720"#),
        "the check sees the frame size the task set: {}",
        facts[0]
    );
    agent.close();
    host.relay.shutdown().await;
}
