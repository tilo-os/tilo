//! Close and Settings changes during real durable dispatch publication.
use super::*;

fn assert_consumed(f: &Fixture, epoch: u64) {
    let reopened = tilo_joined_room::open_file(&f.directory.path().join("native.json")).unwrap();
    let (requirement, _) = f.models.queued.lock().unwrap().clone().unwrap();
    let replayed = reopened
        .admit_local_native_invocation(
            requirement.binding().unwrap(),
            &f.native.certificate,
            &f.native.key,
            |_, _, _| Ok(()),
            || -> Result<(), String> { panic!("durably consumed MODEL invocation replayed") },
        )
        .unwrap_err();
    assert!(
        replayed.contains("already bound or dispatched"),
        "{replayed}"
    );
    assert_eq!(
        reopened
            .native_task_admission_state(room(), &f.task, epoch)
            .unwrap(),
        f.native
            .owner
            .native_task_admission_state(room(), &f.task, epoch)
            .unwrap(),
    );
}

#[tokio::test(flavor = "current_thread")]
async fn publication_rechecks_protected_connection_and_full_profile_before_http() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for (task_source, change) in [false, true].into_iter().flat_map(|task_source| {
                ["disconnect", "profile", "expiry"].map(|change| (task_source, change))
            }) {
                let disconnect = change == "disconnect";
                let f = Fixture::new();
                let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
                if task_source {
                    f.models.task_windows.store(true, Ordering::SeqCst);
                    put_grant(
                        &f,
                        tilo_action_policy::Grant::new(
                            "fixture-model-task",
                            tilo_action_policy::Subject::agent("scribe"),
                            NativeModelContract::task_resource(&room().to_string(), &f.task)
                                .action("requestModel"),
                            tilo_action_policy::KindSet::ALL,
                            tilo_action_policy::Answer::Allow,
                            tilo_action_policy::Subject::person(hex::encode(
                                f.native.certificate.user_public_key,
                            )),
                        ),
                    );
                }
                approve_model(&f);
                let (broker, channel) = launched_channel(&f).await;
                let run = delivered_live(&channel).await;
                let id = channel
                    .claim_native_model(run.run_id, &run.lease)
                    .await
                    .unwrap();
                let epoch = f
                    .native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .controls[&f.task]
                    .run_epoch;
                let (entered, release) = f.writer.hold_next();
                let request = serde_json::to_value(request(id, false)).unwrap();
                let executing = tokio::task::spawn_local(async move {
                    channel.execute_native_model(&request).await
                });
                tokio::time::timeout(Duration::from_secs(3), entered)
                    .await
                    .unwrap()
                    .unwrap();
                // This notification is inside Store::publish, after consume but
                // before durability/install. No prepare notification stands in for it.
                assert!(!executing.is_finished());
                if disconnect {
                    drop(broker);
                    tokio::time::timeout(Duration::from_secs(3), f.models.close_notify.notified())
                        .await
                        .unwrap();
                    assert!(f.models.closed.load(Ordering::SeqCst) > 0);
                } else if change == "expiry" {
                    *f.models.final_time.lock().unwrap() =
                        Some(std::time::SystemTime::now() + Duration::from_secs(100 * 365 * 86400));
                } else {
                    let mut config = agent_provider_config::ProviderConfig::load();
                    config.profiles[0].omit_params.push("top_p".into());
                    config.save().unwrap();
                }
                release.release();
                let reply = tokio::time::timeout(Duration::from_secs(3), executing)
                    .await
                    .unwrap()
                    .unwrap();
                if !disconnect {
                    assert_eq!(reply.unwrap()["response"]["ok"], false);
                }
                queued::assert_no_http(&listener).await;
                // Consumption is durable even when the final connection check fails.
                assert_consumed(&f, epoch);
                drop(f);
            }
        })
        .await;
}
