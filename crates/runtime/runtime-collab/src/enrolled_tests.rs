use super::*;
fn envelope() -> EnrolledTaskEnvelope {
    EnrolledTaskEnvelope {
        version: 1,
        message_id: "message-1".into(),
        task_id: "task-1".into(),
        run_id: "run-1".into(),
        launch_id: "launch-1".into(),
        runtime_epoch: "epoch-1".into(),
        recipient_agent: "agent-1".into(),
        channel_lease: "channel-1".into(),
        history: None,
        task: TaskRunEnvelope {
            version: 1,
            task_id: "task-1".into(),
            title: "Private instruction".into(),
            instruction: None,
            run_id: 9,
            lease: "lease-1".into(),
            planning: TaskRunPlanning::Model,
            execution_budget: None,
            action_context: serde_json::json!({}),
        },
    }
}
#[test]
fn enrolled_recipient_retains_exact_acceptance_and_never_reexecutes_duplicate() {
    let first = envelope();
    let mut custody =
        EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
    let accepted = custody.accept(&first).unwrap();
    assert!(accepted.execute);
    assert_eq!(accepted.acceptance.acceptance_sequence, 1);
    let duplicate = custody.accept(&first).unwrap();
    assert!(!duplicate.execute);
    assert_eq!(accepted.acceptance, duplicate.acceptance);
    let mut changed = first.clone();
    changed.task.title = "Different private instruction".into();
    assert!(custody.accept(&changed).is_err());
    changed = first.clone();
    changed.task.instruction = Some("Different full private instruction".into());
    assert!(custody.accept(&changed).is_err());
    changed = first.clone();
    changed.channel_lease = "other-channel".into();
    assert!(custody.accept(&changed).is_err());
    changed = first.clone();
    changed.task.task_id = "other-task".into();
    assert!(custody.accept(&changed).is_err());
}
#[test]
fn enrolled_budget_is_bounded_and_same_message_cannot_rewrite_it() {
    use collab_types::task_budget::TaskExecutionBudget;
    let mut first = envelope();
    assert!(
        serde_json::to_value(&first).unwrap()["task"]
            .get("execution_budget")
            .is_none()
    );
    first.task.execution_budget = Some(TaskExecutionBudget {
        turn_ms: 900_000,
        total_ms: 7_200_000,
    });
    let mut custody =
        EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
    assert!(custody.accept(&first).unwrap().execute);
    assert!(!custody.accept(&first).unwrap().execute);
    let mut changed = first.clone();
    changed.task.execution_budget = Some(TaskExecutionBudget {
        turn_ms: 1,
        total_ms: 1,
    });
    assert!(custody.accept(&changed).is_err());
    changed.task.execution_budget = Some(TaskExecutionBudget {
        turn_ms: 900_001,
        total_ms: 7_200_000,
    });
    assert!(changed.validate().is_err());
    assert!(custody.accept(&changed).is_err());
}

#[test]
fn enrolled_full_private_input_and_retention_bounds_fail_closed() {
    let mut first = envelope();
    first.task.title = "𐐀".repeat(8000);
    assert!(first.validate().is_ok());
    first.task.title.push('x');
    assert!(first.validate().is_err());
    first = envelope();
    first.task.instruction = Some("𐐀".repeat(8000));
    assert!(first.validate().is_ok());
    assert_eq!(first.task.instruction().chars().count(), 8000);
    first.task.instruction.as_mut().unwrap().push('x');
    assert!(first.validate().is_ok());
    first.task.instruction = Some("x".repeat(32 * 1024 + 1));
    assert!(first.validate().is_err());
    first.task.instruction = Some(" \n\t".into());
    assert!(first.validate().is_err());
    let mut custody =
        EnrolledRecipientCustody::new("agent-1", "launch-1", "epoch-1", "channel-1").unwrap();
    let original = envelope();
    let accepted = custody.accept(&original).unwrap();
    let mut refused = false;
    for index in 2..=1025 {
        let mut next = envelope();
        next.message_id = format!("message-{index}");
        next.task_id = format!("task-{index}");
        next.task.task_id = next.task_id.clone();
        next.run_id = format!("run-{index}");
        next.task.run_id = index;
        next.task.title = "𐐀".repeat(8000);
        if custody.accept(&next).is_err() {
            refused = true;
            break;
        }
    }
    assert!(refused, "retained byte budget must refuse new work");
    let replay = custody.accept(&original).unwrap();
    assert!(!replay.execute);
    assert_eq!(replay.acceptance, accepted.acceptance);
}
#[cfg(feature = "client")]
#[tokio::test(flavor = "current_thread")]
async fn task_run_old_peer_cannot_acknowledge_delivery_from_queue_success() {
    struct OldPeer;
    impl collab_capnp::channel_handle::Server for OldPeer {}
    let channel = ChannelHandle {
        inner: Rc::new(capnp_rpc::new_client(OldPeer)),
    };
    let first = envelope();
    assert!(channel.subscribe_task_runs().await.is_err());
    assert!(
        channel
            .report_task_run(
                first.task.run_id,
                &first.task.lease,
                &TaskRunUpdate::Accepted
            )
            .await
            .is_err()
    );
    assert!(
        channel
            .report_task_run(
                first.task.run_id,
                &first.task.lease,
                &TaskRunUpdate::Succeeded {
                    message: "done".into()
                }
            )
            .await
            .is_err()
    );
}
#[cfg(feature = "client")]
#[tokio::test(flavor = "current_thread")]
async fn task_run_report_preserves_full_private_result_and_bounds_encoded_frame() {
    struct Recipient(Rc<std::cell::Cell<usize>>);
    impl collab_capnp::channel_handle::Server for Recipient {
        fn report_task_run(
            self: Rc<Self>,
            params: collab_capnp::channel_handle::ReportTaskRunParams,
            _results: collab_capnp::channel_handle::ReportTaskRunResults,
        ) -> impl std::future::Future<Output = Result<(), capnp::Error>> + 'static {
            let result = (|| {
                let request = params.get()?;
                assert_eq!(request.get_run_id(), 9);
                assert_eq!(request.get_lease()?.to_str()?, "lease-1");
                let update: TaskRunUpdate =
                    serde_json::from_str(request.get_update_json()?.to_str()?)
                        .map_err(|error| capnp::Error::failed(error.to_string()))?;
                match update {
                    TaskRunUpdate::Succeeded { message } => {
                        assert_eq!(message.chars().count(), 8000);
                    }
                    _ => panic!("unexpected update"),
                }
                self.0.set(self.0.get() + 1);
                Ok(())
            })();
            capnp::capability::Promise::from(result)
        }
    }
    let calls = Rc::new(std::cell::Cell::new(0));
    let channel = ChannelHandle {
        inner: Rc::new(capnp_rpc::new_client(Recipient(calls.clone()))),
    };
    let task = envelope().task;
    for message in ["𐐀".repeat(8000), "\u{0001}".repeat(8000)] {
        channel
            .report_task_run(
                task.run_id,
                &task.lease,
                &TaskRunUpdate::Succeeded { message },
            )
            .await
            .unwrap();
    }
    assert_eq!(calls.get(), 2);
    assert!(
        channel
            .report_task_run(
                task.run_id,
                &task.lease,
                &TaskRunUpdate::Succeeded {
                    message: "x".repeat(64 * 1024)
                }
            )
            .await
            .is_err()
    );
    assert_eq!(
        calls.get(),
        2,
        "oversized encoded update must not reach the peer"
    );
}
#[test]
fn enrolled_protocol_rejects_unknown_fields_and_preserves_legacy_literal() {
    let legacy = r#"{"version":1,"task_id":"task-1","title":"Private instruction","run_id":9,"lease":"lease-1","planning":"model","action_context":{}}"#;
    let task: TaskRunEnvelope = serde_json::from_str(legacy).unwrap();
    assert_eq!(task, envelope().task);
    assert_eq!(serde_json::to_string(&task).unwrap(), legacy);
    assert_eq!(task.instruction(), "Private instruction");
    let mut private = envelope();
    private.task.title = "Public summary".into();
    private.task.instruction = Some("Full private instruction".into());
    let encoded = serde_json::to_string(&private).unwrap();
    let decoded: EnrolledTaskEnvelope = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, private);
    assert_eq!(decoded.task.title, "Public summary");
    assert_eq!(decoded.task.instruction(), "Full private instruction");
    assert!(decoded.validate().is_ok());
    let mut value = serde_json::to_value(envelope()).unwrap();
    value["credential"] = serde_json::json!("not permitted");
    assert!(serde_json::from_value::<EnrolledTaskEnvelope>(value).is_err());
}
