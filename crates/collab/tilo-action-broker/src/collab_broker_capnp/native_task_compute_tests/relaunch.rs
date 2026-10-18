//! A retained certificate selects a relaunch, never an already-ready executor.
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn failed_process_launch_keeps_the_saved_goal_queued_without_a_run() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let launcher = Arc::new(Launcher {
                fail: true,
                ..Default::default()
            });
            let registry = Arc::new(crate::services::NativeTaskExecutorRegistry::default());
            let f = Fixture::with_executor_registry(Some(launcher), Some(registry.clone()));
            let _env = model_owner::configure(&f, "http://127.0.0.1:8123/failed-launch/v1".into());
            f.person_command(WorkspaceTaskCommand::QueueWithMetadata {
                title: "Saved goal".into(),
                assigned_agent: None,
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                metadata: Default::default(),
            })
            .unwrap();
            let saved = f.native.owner.workspace_task_snapshot(room()).unwrap();
            let task = saved
                .tasks
                .iter()
                .find(|task| task.assigned_agent.is_none())
                .unwrap()
                .id
                .clone();
            let result = invoke_hook(
                &f,
                &format!("desktop:{task}"),
                "approve-run",
                Some(serde_json::json!({
                    "agent_id":"scribe", "expected_run_epoch":0, "after_revision":0
                })),
            )
            .await;
            assert!(
                matches!(result, tilo_actions::InvocationUpdate::Failed { .. }),
                "{result:?}"
            );
            assert_eq!(
                f.native.owner.workspace_task_snapshot(room()).unwrap(),
                saved
            );
            assert!(registry.candidates(&room().to_string()).unwrap().is_empty());
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn second_model_goal_and_offline_assignment_wait_for_protected_resubscription() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for assigned in [false, true] {
                let launcher = Arc::new(Launcher::default());
                let registry = Arc::new(crate::services::NativeTaskExecutorRegistry::default());
                let f =
                    Fixture::with_executor_registry(Some(launcher.clone()), Some(registry.clone()));
                let _env = model_owner::configure(&f, "http://127.0.0.1:8123/relaunch/v1".into());
                for goal in 0..2 {
                    let title = format!("Private goal {goal}");
                    f.person_command(WorkspaceTaskCommand::QueueWithMetadata {
                        title: title.clone(),
                        assigned_agent: (goal == 1 && assigned).then(|| "scribe".into()),
                        route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                        metadata: Default::default(),
                    })
                    .unwrap();
                    let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
                    let task = snapshot
                        .tasks
                        .iter()
                        .find(|task| task.title == title)
                        .unwrap()
                        .id
                        .clone();
                    assert!(snapshot.controls[&task].active_run.is_none());
                    let resource = format!("desktop:{task}");
                    let start = invoke_hook(
                        &f,
                        &resource,
                        "approve-run",
                        Some(serde_json::json!({
                            "agent_id":"scribe", "expected_run_epoch":0, "after_revision":0
                        })),
                    );
                    let subscribe = async {
                        // A retained assignment must not bypass this launch rendezvous.
                        launcher.launched.notified().await;
                        let pending = f.native.owner.workspace_task_snapshot(room()).unwrap();
                        assert!(pending.controls[&task].active_run.is_none());
                        let queued = pending.tasks.iter().find(|row| row.id == task).unwrap();
                        assert_eq!(
                            queued.status,
                            tilo_desktop_model::WorkspaceTaskStatus::Queued
                        );
                        assert_eq!(queued.assigned_agent.is_some(), goal == 1 && assigned);
                        let (managed, socket) = managed_channel(&f, 79 + goal);
                        let stream = managed
                            .subscribe_task_runs_request()
                            .send()
                            .promise
                            .await
                            .unwrap()
                            .get()
                            .unwrap()
                            .get_stream()
                            .unwrap();
                        (managed, socket, stream)
                    };
                    let (result, connection) =
                        tokio::time::timeout(Duration::from_secs(10), async {
                            tokio::join!(start, subscribe)
                        })
                        .await
                        .expect("Start must wait for actual protected subscription");
                    assert!(
                        matches!(result, tilo_actions::InvocationUpdate::Succeeded { .. }),
                        "{result:?}"
                    );
                    let current = registry
                        .executor_for_peer(79 + goal, &room().to_string(), "scribe")
                        .unwrap();
                    assert!(registry.is_current(&current));
                    let approved = f.native.owner.workspace_task_snapshot(room()).unwrap();
                    let run = approved.controls[&task].active_run.as_ref().unwrap();
                    assert_eq!(run.agent_id, "scribe");
                    assert_eq!(
                        run.approval.as_ref().unwrap().device,
                        f.native.certificate.device_public_key
                    );
                    assert_ne!(current.certificate, f.native.certificate);
                    // Read the actual admitted stream, not just a Running projection.
                    // The first run can also be redelivered after reconnecting.
                    let mut found = false;
                    for _ in 0..=goal {
                        let reply = tokio::time::timeout(
                            Duration::from_secs(2),
                            connection.2.read_request().send().promise,
                        )
                        .await
                        .unwrap()
                        .unwrap();
                        let delivered: runtime_collab::TaskRunEnvelope = serde_json::from_str(
                            reply
                                .get()
                                .unwrap()
                                .get_task_json()
                                .unwrap()
                                .to_str()
                                .unwrap(),
                        )
                        .unwrap();
                        if delivered.task_id == task {
                            found = true;
                            break;
                        }
                    }
                    assert!(found, "approved goal must reach its authenticated executor");
                    f.state.disconnect_native_task_stream(79 + goal);
                    drop(connection);
                    assert!(!registry.is_current(&current));
                    assert_eq!(registry.candidates(&room().to_string()).unwrap().len(), 1);
                    let sample = f
                        .native
                        .owner
                        .with_current(room(), |view| {
                            registry.sample(
                                view.peer(),
                                view.governor(),
                                &room().to_string(),
                                &f.native.certificate,
                                Some("Configured test model"),
                            )
                        })
                        .unwrap();
                    assert_eq!(sample.len(), 1);
                    assert!(!sample[0].current_certified_executor);
                }
            }
        })
        .await;
}
