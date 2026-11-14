//! A launched task agent takes its task's steps through its run
//! ([`super::TaskRunHost`]), and loses them once its person stops the run.
#![allow(
    clippy::significant_drop_tightening,
    reason = "each scenario holds its connection to the end"
)]

use super::agent_connection::admit;
use super::*;
use tilo_actions::ActionAddress;

/// The run the launched agent works in.
const ISSUE: &str = "tasks.tracker/issues/release-notes";
/// The host steps' provider in this fixture.
const DESKTOP_PROVIDER: &str = "test-desktop";

/// A desktop whose one step lists its windows.
#[derive(Default)]
struct Desktop {
    started: Mutex<Vec<String>>,
    /// The turn each step was handed as its actor's, in order.
    turns: Mutex<Vec<Option<tilo_action_policy::turn::Turn>>>,
    /// Whether its steps are a task agent's steps of its run.
    for_tasks: bool,
    /// Whether its steps are a turn's (the session's own, as a session's shell publishes them).
    for_turns: bool,
}

impl crate::services::HostStepServices for Desktop {
    fn provider(&self) -> &'static str {
        DESKTOP_PROVIDER
    }
    fn app_id(&self) -> &'static str {
        "so.tilo.desktop"
    }
    fn for_tasks(&self) -> bool {
        self.for_tasks
    }
    fn for_turns(&self) -> bool {
        self.for_turns
    }
    fn descriptors(&self) -> Vec<tilo_actions::ActionDescriptor> {
        vec![tilo_actions::ActionDescriptor {
            did: None,
            semantic_identity: None,
            address: ActionAddress::new("desktop", "list-windows"),
            resource_kind: "desktop".into(),
            resource_label: "Desktop".into(),
            action_label: "List windows".into(),
            keywords: Vec::new(),
            icon: None,
            description: None,
            state: tilo_actions::ActionState::default(),
            locus: tilo_actions::ExecutionLocus::default(),
            kind: tilo_actions::OperationKind::View,
            contract: tilo_actions::ActionContract::view(),
            input: tilo_actions::InputSpec::None,
            params: Vec::new(),
            view: None,
        }]
    }
    fn start(
        &self,
        request: &crate::services::HostStepRequest<'_>,
    ) -> Result<tilo_actions::remote::Pending<crate::services::HostStepOutcome>, String> {
        self.started
            .lock()
            .unwrap()
            .push(request.actor.id().to_owned());
        self.turns
            .lock()
            .unwrap()
            .push(request.actor.turn().cloned());
        Ok(tilo_actions::remote::Pending::ready(Ok(
            tilo_actions::ActionOutput::default(),
        )))
    }
}

/// A task agent launched for its task joins over its own channel. A host step marked for tasks (a task's programs) is
/// in its catalogue, and it takes the step as a child of its run: taken once, read
/// back by the run, refused once the run stops.
#[tokio::test(flavor = "current_thread")]
async fn a_launched_task_agent_takes_its_task_steps_in_its_run() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let steps = Arc::new(Desktop {
                for_tasks: true,
                ..Desktop::default()
            });
            let f = Fixture::with_services(None, vec![steps.clone()]);
            let agent = admit(&f);
            f.native.runs.lock().unwrap().push(TrackerRun {
                issue: ISSUE.into(),
                title: "Write the release notes".into(),
                instruction: "Write the release notes.".into(),
                agent: agent.delegation.agent_id.clone(),
                epoch: 1,
                starter: hex::encode(agent.device.user_public_key),
                active: true,
                finished_by_agent: false,
                subject: None,
                turn: None,
                question: None,
                delegation: Some(Ok(agent.delegation.encode())),
                origin: None,
            });
            let route = CrdtRoute::record(
                tilo_actions::remote::task_programs::APP,
                workspace_id().to_string(),
                "",
            );
            let admitted = verify_join_identity(
                &route,
                &agent.presentation(),
                f.state.membership.as_ref(),
                f.state.trusted_devices.as_deref().map(Vec::as_slice),
                f.state.certificate_authority.get().map(Arc::as_ref),
            )
            .unwrap();
            let actor = admitted.broker_actor().working_on(ISSUE);
            let catalogue = f
                .state
                .action_catalogue_for(&workspace_id().to_string(), &actor)
                .unwrap();
            assert!(
                catalogue.iter().any(|surface| surface.provider.as_deref()
                    == Some(DESKTOP_PROVIDER)
                    && surface.actions[0].action == "list-windows"),
                "the task step is in the task agent's catalogue: {catalogue:?}"
            );
            let channel = f.channel_admitted(300, &route, admitted);
            let run = delivered(&channel).await;
            // The step the run takes: its own task action, under the run's lease.
            let task_action = runtime_collab::TaskAction {
                peer_id: 0,
                provider: DESKTOP_PROVIDER,
                resource: "desktop",
                action: "list-windows",
                argument: None,
            };
            let take = |invocation: u128| {
                let channel = channel.clone();
                let (run_id, lease) = (run.run_id, run.lease.clone());
                async move {
                    channel
                        .invoke_task_action(
                            run_id,
                            &lease,
                            task_action,
                            &uuid::Uuid::from_u128(invocation).to_string(),
                        )
                        .await
                }
            };
            let (_id, update) = take(61).await.expect("the run takes its task step");
            let update: tilo_actions::InvocationUpdate =
                tilo_actions::decode_json(update.as_bytes()).unwrap();
            assert!(
                matches!(
                    update,
                    tilo_actions::InvocationUpdate::Accepted { .. }
                        | tilo_actions::InvocationUpdate::Succeeded { .. }
                ),
                "{update:?}"
            );
            assert_eq!(
                steps.started.lock().unwrap().as_slice(),
                [format!("agent:{}", agent.delegation.agent_id)],
                "taken once, by the task agent"
            );
            // Asked again, the same step is read back, not taken again.
            take(61).await.expect("read back");
            assert_eq!(steps.started.lock().unwrap().len(), 1);
            for run in f.native.runs.lock().unwrap().iter_mut() {
                run.active = false;
            }
            f.state.refresh_native_task_delivery();
            assert!(take(62).await.is_err(), "a stopped run takes no step");
            assert_eq!(steps.started.lock().unwrap().len(), 1);
            drop(f);
        })
        .await;
}
