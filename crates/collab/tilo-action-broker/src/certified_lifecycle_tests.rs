// Uses the signed local host and real application stream, not injected job ownership.
#[tokio::test(flavor = "current_thread")]
async fn certified_jobs_lifecycle_uses_exact_certificate_owner_and_live_session() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let env = Environment::new();
            let rig = Rig::new(&env).await;
            let mut action = directory_state("original-r1")
                .borrow()
                .row()
                .descriptor
                .unwrap();
            action.operation_kind = tilo_actions::OperationKind::Effect;
            action.contract = tilo_actions::ActionContract {
                execution: tilo_actions::ExecutionMode::Background { cancellable: true },
                result: tilo_actions::ResultSpec::Job,
                ..tilo_actions::ActionContract::default()
            };
            rig.worker
                .handle
                .publish_actions(&env.route.app_id, &[action])
                .await
                .unwrap();
            let (caller, evidence) = local_host_evidence(&env);
            let host = rig
                .state
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let lifecycle = rig
                .state
                .certified_action_lifecycle(&caller, &evidence)
                .unwrap();
            let request = local_host_request(false);
            let answer = host.invoke(&caller, request.clone());
            rig.state.drain_action_directory_bridge(&rig.directories);
            local_host_answer(answer).await.unwrap();
            let _lease = local_host_lease(&rig, &caller, request.invocation).await;
            let id = rig.state.remote_job(&caller, &request.invocation).unwrap();
            assert_eq!(lifecycle.statuses().len(), 1);
            assert_eq!(lifecycle.statuses()[0].0, id);
            assert_eq!(
                lifecycle.status(id).unwrap().invocation(),
                request.invocation
            );
            assert!(lifecycle.can_cancel(id));
            // The same actor label, without custody, still sees no certified job.
            let actor = hex::encode(env.cert.user_public_key);
            assert!(rig.state.invocation_statuses(&actor).is_empty());
            assert!(!rig.state.can_cancel(id, &actor));
            let (other, other_evidence) = member_device_evidence(&env, 94, false);
            let foreign = rig
                .state
                .certified_action_lifecycle(&other, &other_evidence)
                .unwrap();
            assert!(foreign.statuses().is_empty());
            assert!(foreign.status(id).is_none());
            assert!(!foreign.can_cancel(id));
            assert!(foreign.cancel(id).is_err());
            // Another execution of the exact device can inspect and cancel its job.
            let resumed = rig
                .state
                .certified_action_lifecycle(&caller.clone().in_new_execution(), &evidence)
                .unwrap();
            assert!(resumed.status(id).is_some());
            resumed.cancel(id).unwrap();
            assert!(
                rig.state
                    .cancellation_requests
                    .lock()
                    .unwrap()
                    .contains(&id)
            );
            let update = tilo_actions::InvocationUpdate::Cancelled {
                invocation: request.invocation,
                message: None,
            };
            rig.worker
                .handle
                .update_invocation(id, &serde_json::to_string(&update).unwrap())
                .await
                .unwrap();
            assert_eq!(lifecycle.status(id), Some(update));
            assert_eq!(
                lifecycle.statuses().len(),
                1,
                "terminal rows retain exact ownership"
            );
            assert!(!lifecycle.can_cancel(id));
            assert!(lifecycle.cancel(id).is_err());
            let retired = caller.clone().in_new_execution();
            let ended = rig
                .state
                .certified_action_lifecycle(&retired, &evidence)
                .unwrap();
            retired.retire_execution();
            assert!(ended.statuses().is_empty());
            assert!(ended.status(id).is_none());
            assert!(ended.cancel(id).is_err());
            env.authority
                .additional_certificates
                .lock()
                .unwrap()
                .clear();
            assert!(lifecycle.statuses().is_empty());
            assert!(lifecycle.status(id).is_none());
            assert!(!lifecycle.can_cancel(id));
            assert!(lifecycle.cancel(id).is_err());
            rig.stop().await;
        })
        .await;
}
