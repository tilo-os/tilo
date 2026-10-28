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

/// What one plan of a coordinated task was asked and shown: its text, the
/// lines of what records hold, and what its role was told.
type Asked = Arc<Mutex<Vec<(String, Vec<String>, String)>>>;

/// A model's planner for a coordinated task: the coordinator lists one
/// item, its agent sets the frame size, the check adds a fix-up item, whose
/// agent sets it again. It keeps what each plan was asked and shown.
struct Coordinated {
    asked: Asked,
    obligations: tilo_agent_participant::Obligations,
}

impl Coordinated {
    fn frame(width: u32, height: u32) -> Step {
        let mut arguments = serde_json::Map::new();
        arguments.insert("width".into(), width.into());
        arguments.insert("height".into(), height.into());
        Step {
            app: VIDEO.into(),
            record: None,
            action: StepAction::Invoke {
                address: ActionAddress::new("video-editor.project", "set-frame-size"),
                arguments,
            },
            carries: None,
            serves: None,
        }
    }

    fn items(text: &str, app: &str) -> tilo_agent_participant::Obligations {
        tilo_agent_participant::Obligations {
            items: vec![text.into()],
            plans: vec![tilo_agent::tools::ItemPlan {
                apps: vec![app.into()],
                after: Vec::new(),
                wait: None,
            }],
            ..tilo_agent_participant::Obligations::default()
        }
    }
}

impl Planner for Coordinated {
    fn plan(&mut self, task: &str, context: &PlanContext) -> Result<Vec<Step>, String> {
        let mut asked = self.asked.lock().unwrap();
        asked.push((
            task.to_owned(),
            context.snapshots.clone(),
            context.role.note().to_owned(),
        ));
        match asked.len() {
            1 => {
                self.obligations = Self::items("set the frame size", "video-editor");
                Ok(Vec::new())
            }
            2 => Ok(vec![Self::frame(1280, 720)]),
            3 => {
                self.obligations = Self::items("set the frame size to 1920x1080", VIDEO);
                Ok(Vec::new())
            }
            4 => Ok(vec![Self::frame(1920, 1080)]),
            _ => Err("planned once too often".into()),
        }
    }

    fn checks_work(&self) -> bool {
        true
    }

    fn last_planned_by_model(&self) -> bool {
        true
    }

    fn take_obligations(&mut self) -> tilo_agent_participant::Obligations {
        std::mem::take(&mut self.obligations)
    }

    fn decomposes(&self) -> bool {
        true
    }
}

/// A coordinated task is checked once, by its coordinator, shown the facts
/// of the records its items changed; the check's fix-up item is done by a
/// fresh agent, and there is no second check. The item's agent itself is
/// not checked.
/// An agent, Ed, granted the video editor and planning with `planner`, in a
/// room on a host of its own, admitted: the host, its steward, the host's
/// state, the agent and its steering.
async fn ed_planning_with(
    planner: Box<dyn Planner>,
) -> (
    support::TestHost,
    steward::Steward,
    tempfile::TempDir,
    AgentParticipant,
    Arc<AgentControl>,
) {
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
    let steward = steward::Steward::start(&host.relay, &root).await;
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
        now.saturating_add(3_600_000_000_000),
    )
    .unwrap();
    host.relay.register_agent(delegation.clone()).unwrap();
    let control = AgentControl::new(delegation.clone());
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
        planner,
    )
    .unwrap();
    wait_for("the agent to be admitted", || {
        agent.pump();
        let waiting = control.status() == AgentStatus::Waiting;
        async move { waiting.then_some(()) }
    })
    .await;

    (host, steward, state, agent, control)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_coordinator_s_check_sees_the_facts_and_adds_one_fix() {
    let asked: Asked = Arc::new(Mutex::new(Vec::new()));
    let (host, _steward, _state, mut agent, control) =
        Box::pin(ed_planning_with(Box::new(Coordinated {
            asked: Arc::clone(&asked),
            obligations: tilo_agent_participant::Obligations::default(),
        })))
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

    let asked = asked.lock().unwrap().clone();
    assert_eq!(
        asked.len(),
        4,
        "the list, the item, the check, the fix: {asked:?}"
    );
    assert!(
        asked[0].2.contains("List the request's items"),
        "{:?}",
        asked[0]
    );
    assert_eq!(asked[1].0, "set the frame size");
    assert!(asked[1].2.contains("you are the agent of item 1 only"));
    let (check, shown, role) = &asked[2];
    assert!(check.starts_with("Every item has ended"), "{check}");
    assert!(role.contains("1. set the frame size (done)"), "{role}");
    let facts: Vec<&String> = shown
        .iter()
        .filter(|line| line.contains("video-editor.project#facts states it now"))
        .collect();
    assert_eq!(facts.len(), 1, "{shown:?}");
    assert!(
        facts[0].contains(r#""width":1280"#) && facts[0].contains(r#""height":720"#),
        "the check sees the frame size the item set: {}",
        facts[0]
    );
    assert_eq!(asked[3].0, "set the frame size to 1920x1080");
    assert!(
        asked[3].2.contains("you are the agent of item 2 only")
            && asked[3]
                .2
                .contains("- 1. set the frame size: done. Ran: set-frame-size"),
        "{}",
        asked[3].2
    );
    agent.close();
    host.relay.shutdown().await;
}
