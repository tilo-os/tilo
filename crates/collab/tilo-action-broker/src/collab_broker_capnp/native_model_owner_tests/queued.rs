//! Queued production HTTP attempts retain no authority across owner turns.
use super::*;

/// No request is inferred from a dispatch counter: inspect the transport itself.
pub(super) async fn assert_no_http(listener: &tokio::net::TcpListener) {
    while let Ok(accepted) =
        tokio::time::timeout(Duration::from_millis(100), listener.accept()).await
    {
        let (mut socket, _) = accepted.unwrap();
        let mut bytes = Vec::new();
        tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
            .await
            .unwrap()
            .unwrap();
        assert!(bytes.is_empty(), "denied MODEL attempt sent HTTP bytes");
    }
}

struct HeldAuthority {
    lease: Arc<dyn NativeModelLease>,
    contract: NativeModelContract,
    entered: Arc<tokio::sync::Notify>,
    release: Mutex<std::sync::mpsc::Receiver<()>>,
}
impl NativeModelAuthority for HeldAuthority {
    fn current_model_context(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        now: std::time::SystemTime,
    ) -> Result<NativeModelContext, String> {
        self.entered.notify_one();
        self.release
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(3))
            .map_err(|_| "test did not release unrelated owner transaction")?;
        self.lease.current(view, self.contract.clone(), now)
    }
}

struct HeldTransaction {
    entered: Arc<tokio::sync::Notify>,
    release: std::sync::mpsc::Sender<()>,
    work: tokio::task::JoinHandle<Result<(), String>>,
}

fn hold_unrelated_transaction(f: &Fixture) -> HeldTransaction {
    let (mut other, lease) = f.models.queued.lock().unwrap().clone().unwrap();
    // A separate typed reservation holds the real owner's mutation turn,
    // not a fixture mutex masquerading as the owner's dispatch gate.
    other.invocation = uuid::Uuid::new_v4();
    other.request = uuid::Uuid::new_v4();
    let entered = Arc::new(tokio::sync::Notify::new());
    let (release, held) = std::sync::mpsc::channel();
    let authority = HeldAuthority {
        lease,
        contract: other.modelselection.clone(),
        entered: entered.clone(),
        release: Mutex::new(held),
    };
    let native = f.native.clone();
    let work = tokio::task::spawn_blocking(move || {
        native
            .owner
            .reserve_native_model(
                &other,
                NativeModelGovernorCustody {
                    host: &native.certificate,
                    key: &native.key,
                    now: std::time::SystemTime::now(),
                },
                &authority,
            )
            .map(|_| ())
    });
    HeldTransaction {
        entered,
        release,
        work,
    }
}

#[tokio::test(flavor = "current_thread")]
async fn queued_http_waits_across_unrelated_owner_transaction_and_stop_host_ack() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
            approve_model(&f);
            f.models.pause.store(true, Ordering::SeqCst);
            let (_broker, channel) = launched_channel(&f).await;
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
            let request = serde_json::to_value(request(id, false)).unwrap();
            let executing =
                tokio::task::spawn_local(
                    async move { channel.execute_native_model(&request).await },
                );
            tokio::time::timeout(Duration::from_secs(3), f.models.prepared.notified())
                .await
                .unwrap();
            let HeldTransaction {
                entered,
                release,
                work: unrelated,
            } = hold_unrelated_transaction(&f);
            tokio::time::timeout(Duration::from_secs(3), entered.notified())
                .await
                .unwrap();
            assert!(
                !executing.is_finished(),
                "queued HTTP escaped its custody wait"
            );
            let native = f.native.clone();
            let task = f.task.clone();
            let stop_entered = Arc::new(tokio::sync::Notify::new());
            let started = stop_entered.clone();
            let stopping = tokio::task::spawn_blocking(move || {
                started.notify_one();
                let fence =
                    native
                        .owner
                        .stop_native_admission(room(), &task, epoch, &native.key)?;
                native
                    .owner
                    .acknowledge_native_host_fence(&fence, &native.certificate, &native.key)
            });
            tokio::time::timeout(Duration::from_secs(3), stop_entered.notified())
                .await
                .unwrap();
            assert!(
                !stopping.is_finished(),
                "Stop bypassed the actual owner transaction"
            );
            release.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(3), unrelated)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            tokio::time::timeout(Duration::from_secs(3), stopping)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            // Release the already claimed HTTP attempt only after both durable
            // fences exist. The real typed final gate must still refuse transfer.
            f.models.resume.notify_one();
            let reply = tokio::time::timeout(Duration::from_secs(3), executing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(reply["response"]["ok"], false, "{reply}");
            assert_no_http(&listener).await;
            let restarted =
                tilo_joined_room::open_file(&f.directory.path().join("native.json")).unwrap();
            assert_eq!(
                restarted
                    .native_task_admission_state(room(), &f.task, epoch)
                    .unwrap(),
                f.native
                    .owner
                    .native_task_admission_state(room(), &f.task, epoch)
                    .unwrap(),
                "the stop/host acknowledgement must survive owner restart",
            );
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn changed_full_profile_after_claim_cannot_send_to_either_endpoint() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let original = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let changed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _env = configure(&f, format!("http://{}/v1", original.local_addr().unwrap()));
            approve_model(&f);
            let (_broker, channel) = launched_channel(&f).await;
            let run = delivered_live(&channel).await;
            for transform in [false, true] {
                let id = channel
                    .claim_native_model(run.run_id, &run.lease)
                    .await
                    .unwrap();
                let mut config = agent_provider_config::ProviderConfig::load();
                let saved = config.profiles[0].clone();
                config.profiles[0].endpoint =
                    format!("http://{}/v1", changed.local_addr().unwrap());
                config.profiles[0].omit_params.push("temperature".into());
                config.save().unwrap();
                let request = serde_json::to_value(request(id, transform)).unwrap();
                let reply = channel.execute_native_model(&request).await.unwrap();
                assert_eq!(reply["response"]["ok"], false, "{reply}");
                assert!(
                    channel.execute_native_model(&request).await.is_err(),
                    "denial must consume invocation custody"
                );
                config.profiles[0] = saved;
                config.save().unwrap();
            }
            assert_no_http(&original).await;
            assert_no_http(&changed).await;
            drop(f);
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn replaced_run_epoch_refuses_old_model_identity_without_http() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
            approve_model(&f);
            let (_broker, channel) = launched_channel(&f).await;
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
            let authority = WorkspaceTaskAuthority::Host {
                attestor: f.native.certificate.clone(),
            };
            f.native
                .owner
                .commit_workspace_task_command_with_key(
                    OwnerCommitRequest {
                        room: room().to_string(),
                        operation: CommitOperation::WorkspaceTaskCommand {
                            invocation: uuid::Uuid::new_v4(),
                            authority,
                            command: WorkspaceTaskCommand::Abandon {
                                task_id: f.task.clone(),
                                run_epoch: epoch,
                                reason: "Host restarted".into(),
                            },
                        },
                    },
                    &f.native.key,
                    std::time::SystemTime::now(),
                )
                .unwrap();
            f.person_command(WorkspaceTaskCommand::Retry {
                task_id: f.task.clone(),
                run_epoch: epoch,
            })
            .unwrap();
            f.person_command(WorkspaceTaskCommand::ApproveRun {
                task_id: f.task.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: epoch,
                after_revision: 0,
            })
            .unwrap();
            assert!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .controls[&f.task]
                    .run_epoch
                    > epoch
            );
            let request = serde_json::to_value(request(id, false)).unwrap();
            if let Ok(reply) = channel.execute_native_model(&request).await {
                assert_eq!(
                    reply["response"]["ok"], false,
                    "old epoch reached MODEL: {reply}"
                );
            }
            assert_no_http(&listener).await;
            drop(f);
        })
        .await;
}
