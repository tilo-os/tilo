//! Restart the durable owner and protected broker, not just an HTTP fixture.
use super::*;

#[tokio::test(flavor = "current_thread")]
async fn owner_and_broker_restart_cannot_replay_or_reclaim_stopped_model_http() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let mut f = Fixture::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
            approve_model(&f);
            let (broker, channel) = launched_channel(&f).await;
            let run = delivered_live(&channel).await;
            let epoch = f
                .native
                .owner
                .workspace_task_snapshot(room())
                .unwrap()
                .controls[&f.task]
                .run_epoch;
            let id = channel
                .claim_native_model(run.run_id, &run.lease)
                .await
                .unwrap();
            let old_request = serde_json::to_value(request(id, false)).unwrap();
            f.models.pause.store(true, Ordering::SeqCst);
            let pending_channel = channel.clone();
            let pending_request = old_request.clone();
            let pending = tokio::task::spawn_local(async move {
                pending_channel.execute_native_model(&pending_request).await
            });
            tokio::time::timeout(Duration::from_secs(3), f.models.prepared.notified())
                .await
                .unwrap();
            let fence = f
                .native
                .owner
                .stop_native_admission(room(), &f.task, epoch, &f.native.key)
                .unwrap();
            f.native
                .owner
                .acknowledge_native_host_fence(&fence, &f.native.certificate, &f.native.key)
                .unwrap();
            f.models.resume.notify_one();
            let denied = tokio::time::timeout(Duration::from_secs(3), pending)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(denied["response"]["ok"], false, "{denied}");
            drop(channel);
            drop(broker);
            let owner =
                tilo_joined_room::open_file(&f.directory.path().join("native.json")).unwrap();
            assert_eq!(
                owner
                    .native_task_admission_state(room(), &f.task, epoch)
                    .unwrap(),
                tilo_joined_room::task_admission::NativeTaskAdmissionState::Stopped
            );
            f.native = Arc::new(Native {
                owner,
                key: f.native.key.clone(),
                certificate: f.native.certificate.clone(),
                origin: OwnerPolicyClock::new().unwrap().new_origin(),
                cached_projection: Mutex::new(None),
                capped_connection: std::sync::atomic::AtomicBool::new(false),
                spent_grants: Mutex::new(Vec::new()),
                inbox_observed: tokio::sync::Notify::new(),
                start_models: Mutex::new(None),
            });
            f.models = Arc::new(Models::new(f.native.clone()));
            let (_broker, channel) = launched_channel(&f).await;
            assert!(
                channel.execute_native_model(&old_request).await.is_err(),
                "restarting a broker must not restore a consumed connection capability"
            );
            // A newly delivered lease and freshly minted model identity are not a
            // way around the owner's retained stop/host-ack ledger either.
            let restarted_run = delivered_live(&channel).await;
            for transform in [false, true] {
                let id = channel
                    .claim_native_model(restarted_run.run_id, &restarted_run.lease)
                    .await
                    .unwrap();
                let request = serde_json::to_value(request(id, transform)).unwrap();
                let reply = channel.execute_native_model(&request).await.unwrap();
                assert_eq!(reply["response"]["ok"], false, "{reply}");
            }
            queued::assert_no_http(&listener).await;
        })
        .await;
}
