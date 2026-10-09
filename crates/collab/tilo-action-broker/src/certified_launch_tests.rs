// Real signed caller, actual private worker RPC publication and original SDK
// authorizer, with only the host launcher replaced by a bounded recording gate.
fn certified_launch_action() -> runtime_collab::ExportedAction {
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
    action
}

async fn certified_launch_target(
    rig: &Rig,
    env: &Environment,
    retained: bool,
    waiting: bool,
) -> String {
    rig.worker
        .handle
        .publish_actions(&env.route.app_id, &[certified_launch_action()])
        .await
        .unwrap();
    let surface = rig
        .state
        .published_surfaces()
        .into_iter()
        .find(|surface| surface.peer_id == 903 && surface.live)
        .unwrap();
    let provider = if retained {
        super::launchable::launchable_provider(&surface)
    } else {
        tilo_actions::remote::application_action_target(
            &env.route.cluster_id,
            &env.route.app_id,
            "workspace:atlas",
            "open",
        )
        .provider
    };
    if waiting {
        // Withdraw the live offer over the real worker RPC. Restore only its
        // earlier actual published launch metadata, as retained startup state.
        rig.worker
            .handle
            .publish_actions(&env.route.app_id, &[])
            .await
            .unwrap();
        rig.state.launchable.retain(&surface);
    }
    provider
}

fn certified_launch_gate(
    rig: &Rig,
) -> (
    std::sync::mpsc::Receiver<()>,
    std::sync::mpsc::Sender<()>,
    Arc<std::sync::atomic::AtomicUsize>,
) {
    let (started, started_rx) = std::sync::mpsc::channel();
    let (release, released) = std::sync::mpsc::channel();
    let released = Mutex::new(released);
    let launches = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let count = launches.clone();
    rig.state.launchable.replace_launcher_for_test(
        Arc::new(move |app, workspace| {
            assert_eq!(app, "so.tilo.word-processor");
            assert!(!workspace.room.is_nil());
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            started.send(()).map_err(|error| error.to_string())?;
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .map_err(|error| error.to_string())
        }),
        Duration::from_secs(5),
    );
    (started_rx, release, launches)
}

async fn certified_launch_wait(started: &std::sync::mpsc::Receiver<()>) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match started.try_recv() {
                Ok(()) => break Ok(()),
                Err(std::sync::mpsc::TryRecvError::Empty) => tokio::task::yield_now().await,
                Err(error) => break Err(error),
            }
        }
    })
    .await
    .expect("launcher starts within the bound")
    .expect("launcher remains open");
}

#[tokio::test(flavor = "current_thread")]
async fn certified_launch_aliases_preserve_actual_worker_origin_and_original_request_deduplication()
{
    tokio::task::LocalSet::new()
        .run_until(async {
            for retained in [false, true] {
                for waiting in [false, true] {
                    let env = Environment::new();
                    let rig = Rig::new(&env).await;
                    let provider = certified_launch_target(&rig, &env, retained, waiting).await;
                    let (started, release, launches) = certified_launch_gate(&rig);
                    let (caller, evidence) = local_host_evidence(&env);
                    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(
                        rig.state.clone(),
                    );
                    let host = facade
                        .certified_local_action_host(&caller, &evidence)
                        .unwrap();
                    let mut request = local_host_request(false);
                    request.target.provider = provider;
                    let mut events = rig.state.subscribe_execution_activity();
                    let pending = host.invoke(&caller, request.clone());
                    if waiting {
                        certified_launch_wait(&started).await;
                        rig.worker
                            .handle
                            .publish_actions(&env.route.app_id, &[certified_launch_action()])
                            .await
                            .unwrap();
                        release.send(()).unwrap();
                    }
                    let receipt = local_host_answer(pending).await.unwrap();
                    assert_eq!(receipt.invocation, request.invocation);
                    let lease = local_host_lease(&rig, &caller, request.invocation).await;
                    lease.check().unwrap();
                    let event = events.try_recv().unwrap();
                    assert_eq!(event.peer_id, 903);
                    assert_eq!(event.route, env.route);
                    assert_eq!(event.invocation, request.invocation);
                    let job = rig.state.remote_job(&caller, &request.invocation).unwrap();
                    assert_eq!(
                        rig.state
                            .pending_invocations
                            .lock()
                            .unwrap()
                            .get(&job)
                            .unwrap()
                            .peer_id,
                        903
                    );
                    assert_eq!(
                        local_host_answer(host.invoke(&caller, request.clone()))
                            .await
                            .unwrap(),
                        receipt
                    );
                    // Resolution does not replace the original request's dedupe key.
                    request.target.provider = "903".into();
                    assert!(
                        local_host_answer(host.invoke(&caller, request))
                            .await
                            .is_err()
                    );
                    no_delivery(&rig).await;
                    assert!(events.try_recv().is_err());
                    assert_eq!(
                        launches.load(std::sync::atomic::Ordering::SeqCst),
                        usize::from(waiting)
                    );
                    rig.stop().await;
                }
            }
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn certified_launch_wait_refuses_retirement_certificate_expiry_and_origin_loss_before_delivery()
 {
    tokio::task::LocalSet::new()
        .run_until(async {
            for retained in [false, true] {
                for refusal in 0..4 {
                    let env = Environment::new();
                    let rig = Rig::new(&env).await;
                    let provider = certified_launch_target(&rig, &env, retained, true).await;
                    let (started, release, launches) = certified_launch_gate(&rig);
                    let (caller, evidence) = local_host_evidence(&env);
                    let facade = crate::collab_broker::AppActionBroker::from_state_for_test(
                        rig.state.clone(),
                    );
                    let host = facade
                        .certified_local_action_host(&caller, &evidence)
                        .unwrap();
                    let mut request = local_host_request(false);
                    request.target.provider = provider;
                    let mut events = rig.state.subscribe_execution_activity();
                    let pending = host.invoke(&caller, request.clone());
                    certified_launch_wait(&started).await;
                    // Make the actual receiver ready while the host launcher remains
                    // blocked; the next delivery attempt must recheck original custody.
                    rig.worker
                        .handle
                        .publish_actions(&env.route.app_id, &[certified_launch_action()])
                        .await
                        .unwrap();
                    match refusal {
                        0 => caller.retire_execution(),
                        1 => env
                            .authority
                            .additional_certificates
                            .lock()
                            .unwrap()
                            .clear(),
                        2 => {
                            env.control.set_wall_ns(1);
                            env.control.advance_monotonic(Duration::from_secs(7));
                        }
                        _ => env.origin.invalidate(),
                    }
                    release.send(()).unwrap();
                    assert!(local_host_answer(pending).await.is_err());
                    no_delivery(&rig).await;
                    assert!(
                        events.try_recv().is_err(),
                        "a refused launch wait never accepted an invocation"
                    );
                    assert!(rig.state.pending_invocations.lock().unwrap().is_empty());
                    assert_eq!(launches.load(std::sync::atomic::Ordering::SeqCst), 1);
                    rig.stop().await;
                }
            }
        })
        .await;
}
