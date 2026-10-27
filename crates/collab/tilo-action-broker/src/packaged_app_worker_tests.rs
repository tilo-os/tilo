// An app the shell launched is the worker of its own routes: what it edits
// is admitted as its own actions, attributed to its person and to it.
// Included beside the local document report tests, whose rig it shares.

/// The launches this test process stands for, as the shell records the
/// apps it starts before releasing their launcher.
struct LaunchedHere {
    app: String,
    room: String,
}
impl crate::services::LauncherServices for LaunchedHere {
    fn applications(&self) -> Vec<crate::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("this broker launches nothing".into())
    }
    fn attest_process(&self, pid: u32, app: &str, room: &str) -> bool {
        pid == std::process::id() && app == self.app && room == self.room
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        room: &str,
    ) -> Result<Option<crate::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, room).then_some(
            crate::services::AuthenticatedApplicationLaunch {
                creation: None,
                continuation_instance: None,
                launch_ticket: None,
            },
        ))
    }
}

/// Connect as the launcher connects the app it starts: its marker and
/// handoff first, then the app's own Cap'n Proto session.
fn launched_app_connection(
    state: &Arc<CapnpBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    peer: u64,
    app: &str,
    room: &str,
) -> (
    runtime_collab::CollabClient,
    tokio::task::JoinHandle<Result<(), capnp::Error>>,
    tokio::task::JoinHandle<()>,
) {
    use std::io::Write as _;
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": app,
        "workspace_id": room,
    }))
    .unwrap();
    client
        .write_all(&launcher::services::COLLAB_LAUNCH_MAGIC)
        .unwrap();
    client
        .write_all(
            &launcher::services::COLLAB_LAUNCH_FRAMING
                .encode(&handoff)
                .unwrap(),
        )
        .unwrap();
    rpc_pair(
        state.clone(),
        peer,
        server,
        client,
        None,
        directories.clone(),
    )
}

/// A report of one edit on `route`, made by `env`'s person.
fn edit_report(env: &Environment, route: &CrdtRoute) -> runtime_collab::LocalDocumentActionReport {
    let mut request = crate::local_document_action::Request {
        custody: Some(runtime_collab::committed_document::ConnectionSnapshot {
            root: env.cert.user_public_key,
            device: env.cert.device_public_key,
            agent: None,
            app_id: route.app_id.clone(),
            room: route.cluster_id.clone(),
            document: route.record_id.clone(),
        }),
        version: 1,
        invocation: uuid::Uuid::nil(),
        capture_id: uuid::Uuid::new_v4(),
        revision: "original-r1".into(),
        resource: "workspace:atlas".into(),
        action: "open".into(),
        argument: None,
        encoded: tilo_actions::EncodedAction {
            type_id: "example.open".into(),
            payload: br#"{"typed":"hello"}"#.to_vec(),
        },
        context: None,
        original_heads: vec![[0; 32]],
        mutation_hashes: vec![[1; 32]],
        group: None,
    };
    request.invocation = request.bound_invocation().unwrap();
    serde_json::from_value(serde_json::to_value(&request).unwrap()).unwrap()
}

/// Typing in an app the shell launched is history like any worker's: the
/// edit is admitted, its receipt names the person and the app's own
/// connection. A route of another app on that connection is not its own
/// and is refused definitively, and every route goes with the connection.
#[tokio::test(flavor = "current_thread")]
async fn an_app_the_shell_launched_is_the_worker_of_its_own_routes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            const APP_PEER: u64 = 905;
            let env = Environment::new();
            let mut services = env.membership.services();
            services.app_action_grants = Some(Arc::new(LocalReportGrants(env.membership.clone())));
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                room: env.route.cluster_id.clone(),
            }));
            let rig = Rig::with_services(&env, services).await;
            let (app, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                APP_PEER,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let identity = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let own = app
                .join_channel(&env.route.channel_key(), &identity)
                .await
                .unwrap();
            let worker = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .get(APP_PEER, &env.route)
                .cloned()
                .expect("the launched app is the worker of its own route");
            assert!(worker.host_scope.is_none());
            let registration = worker
                .worker_registration
                .expect("registered for the app its launcher vouched for");
            assert_eq!(registration.route, env.route);
            assert_eq!(registration.certificate, env.cert);

            own.publish_action_directory(
                &env.route.app_id,
                capnp_rpc::new_client(LocalDocumentDirectory {
                    kind: tilo_actions::OperationKind::Document,
                    read: false,
                    pin_dropped: Rc::new(std::cell::Cell::new(false)),
                    resolutions: Rc::new(std::cell::Cell::new(0)),
                }),
            )
            .await
            .unwrap();
            let person = tilo_action_policy::Subject::Person(principal_id_for_root(
                env.cert.user_public_key,
            ));
            let grant = tilo_action_policy::Grant::new(
                "the-person-edits",
                person.clone(),
                env.membership.read().workspaces[0]
                    .node()
                    .app(env.route.app_id.clone()),
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                person,
            );
            env.membership.write().workspaces[0].grant(grant);
            let report = edit_report(&env, &env.route);
            own.admit_local_document_action(&report)
                .await
                .expect("the launched app's edit is admitted");
            {
                let journal = rig.state.journal.lock().unwrap();
                let journal = journal.as_ref().unwrap().as_ref().unwrap();
                let key = InvocationKey {
                    room: env.route.cluster_id.clone(),
                    host: journal.host(),
                    invocation: tilo_actions::InvocationId(report.invocation),
                    owner: InvocationOwner::AuthenticatedLocal {
                        root: env.cert.user_public_key,
                        device: env.cert.device_public_key,
                        agent: None,
                    },
                };
                let receipts = rig.state.local_report_receipts.lock().unwrap();
                let record = receipts.lookup(&key).expect("the edit has a receipt");
                assert_eq!(
                    record.peer_id(),
                    APP_PEER,
                    "attributed to the app's connection"
                );
                assert_eq!(record.intent["app_id"], env.route.app_id);
            }

            // Another app's route in the same room: the connection may join
            // it, but it is not this app's to speak for.
            let other = CrdtRoute::record("so.tilo.notes", env.route.cluster_id.clone(), "notes-1");
            let foreign = app
                .join_channel(&other.channel_key(), &identity)
                .await
                .unwrap();
            assert!(
                rig.state
                    .host_workers
                    .lock()
                    .unwrap()
                    .holder_of(&other)
                    .is_none(),
                "no worker speaks for another app's route"
            );
            let refused = foreign
                .admit_local_document_action(&edit_report(&env, &other))
                .await
                .expect_err("another app's route reports nothing on this connection");
            assert!(
                matches!(
                    &refused,
                    runtime_collab::LocalDocumentActionError::Rejected(reason)
                        if reason == runtime_collab::LOCAL_DOCUMENT_CONNECTION_REPORTS_NOTHING
                ),
                "{refused:?}"
            );

            rig.state.remove_connection(APP_PEER);
            assert!(!rig.state.host_workers.lock().unwrap().holds(APP_PEER));
            drop((own, foreign, app));
            rig.stop().await;
        })
        .await;
}

/// A session opens many windows of one app, each holding several records:
/// every window is the worker of every record it joined, however many the
/// session holds in all, and closing a window withdraws only its own.
#[tokio::test(flavor = "current_thread")]
async fn a_session_with_many_windows_and_records_keeps_every_worker() {
    tokio::task::LocalSet::new()
        .run_until(async {
            const WINDOWS: usize = 48;
            const RECORDS: usize = 6;
            let env = Environment::new();
            let mut services = env.membership.services();
            services.app_action_grants = Some(Arc::new(LocalReportGrants(env.membership.clone())));
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                room: env.route.cluster_id.clone(),
            }));
            let rig = Rig::with_services(&env, services).await;
            let person = tilo_action_policy::Subject::Person(principal_id_for_root(
                env.cert.user_public_key,
            ));
            let grant = tilo_action_policy::Grant::new(
                "the-person-edits",
                person.clone(),
                env.membership.read().workspaces[0]
                    .node()
                    .app(env.route.app_id.clone()),
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                person,
            );
            env.membership.write().workspaces[0].grant(grant);
            let identity = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let route = |window: u64, record: usize| {
                CrdtRoute::record(
                    &env.route.app_id,
                    env.route.cluster_id.clone(),
                    format!("window-{window}-record-{record}"),
                )
            };
            let before = rig.state.host_workers.lock().unwrap().len();
            let mut windows = Vec::new();
            for window in 0..WINDOWS as u64 {
                let peer = 1_000 + window;
                let (app, server, client) = launched_app_connection(
                    &rig.state,
                    &rig.directories,
                    peer,
                    &env.route.app_id,
                    &env.route.cluster_id,
                );
                let mut channels = Vec::new();
                for record in 0..RECORDS {
                    channels.push(
                        app.join_channel(&route(window, record).channel_key(), &identity)
                            .await
                            .expect("a window joins every record it opens"),
                    );
                }
                channels[RECORDS - 1]
                    .publish_action_directory(
                        &env.route.app_id,
                        capnp_rpc::new_client(LocalDocumentDirectory {
                            kind: tilo_actions::OperationKind::Document,
                            read: false,
                            pin_dropped: Rc::new(std::cell::Cell::new(false)),
                            resolutions: Rc::new(std::cell::Cell::new(0)),
                        }),
                    )
                    .await
                    .unwrap();
                windows.push((peer, app, server, client, channels));
            }
            {
                let workers = rig.state.host_workers.lock().unwrap();
                for (peer, ..) in &windows {
                    for record in 0..RECORDS {
                        let window = peer - 1_000;
                        let route = route(window, record);
                        let holder = workers
                            .holder_of(&route)
                            .expect("every record a window holds has its worker");
                        assert_eq!(rig.state.connection_of(holder), *peer);
                        assert!(
                            workers
                                .get(holder, &route)
                                .is_some_and(|worker| worker.worker_registration.is_some()),
                            "window {window} is the worker of record {record}"
                        );
                    }
                }
                assert_eq!(workers.len(), before + WINDOWS * RECORDS);
            }
            for (peer, _, _, _, channels) in &windows {
                channels[RECORDS - 1]
                    .admit_local_document_action(&edit_report(
                        &env,
                        &route(peer - 1_000, RECORDS - 1),
                    ))
                    .await
                    .expect("every window's edit is admitted");
            }

            let (closed, ..) = windows.remove(0);
            rig.state.remove_connection(closed);
            {
                let workers = rig.state.host_workers.lock().unwrap();
                for record in 0..RECORDS {
                    assert!(workers.holder_of(&route(closed - 1_000, record)).is_none());
                }
                assert_eq!(workers.len(), before + (WINDOWS - 1) * RECORDS);
            }
            let (open, _, _, _, channels) = &windows[0];
            channels[RECORDS - 1]
                .admit_local_document_action(&edit_report(&env, &route(open - 1_000, RECORDS - 1)))
                .await
                .expect("the other windows keep their workers");
            drop(windows);
            rig.stop().await;
        })
        .await;
}

/// One window holding three records publishes for each of them: every
/// record is its own root and canonical publisher, another device of the
/// person reaches each one's own invocation stream, and closing one record
/// withdraws only what it published.
#[tokio::test(flavor = "current_thread")]
async fn a_window_publishes_every_record_it_holds_and_withdraws_one_it_drops() {
    use tilo_actions::remote::{
        CanonicalActionRoute, CatalogueQuery, InvokeArgument, InvokeRequest, RemoteActionTarget,
    };
    tokio::task::LocalSet::new()
        .run_until(async {
            const WINDOW: u64 = 907;
            let env = Environment::new();
            let mut services = env.membership.services();
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                room: env.route.cluster_id.clone(),
            }));
            let rig = Rig::with_services(&env, services).await;
            let (window, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                WINDOW,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let identity = presentation(&env.route.cluster_id, &env.cert, &env.key);
            let record =
                |id: &str| CrdtRoute::record(&env.route.app_id, env.route.cluster_id.clone(), id);
            let mut pages = Vec::new();
            for id in ["page-a", "page-b", "page-c"] {
                let channel = window
                    .join_channel(&record(id).channel_key(), &identity)
                    .await
                    .unwrap();
                channel
                    .publish_action_directory(
                        &env.route.app_id,
                        super::signed_worker::directory_server(&directory_state(&format!(
                            "{id}-r1"
                        ))),
                    )
                    .await
                    .unwrap();
                let stream = channel.subscribe_stable_invocations().await.unwrap();
                pages.push((id, channel, stream));
            }
            let roots = |state: &CapnpBrokerState| {
                state
                    .action_directory_roots()
                    .into_iter()
                    .filter(|root| state.connection_of(root.peer_id) == WINDOW)
                    .map(|root| root.record_id)
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(
                roots(&rig.state),
                ["page-a", "page-b", "page-c"].map(String::from).into(),
                "the window publishes a directory for every record it holds"
            );

            let (caller, evidence) = member_device_evidence(&env, 97, false);
            let host = rig
                .state
                .certified_local_action_host(&caller, &evidence)
                .unwrap();
            let canonical = |id: &str| CatalogueQuery {
                canonical_route: Some(CanonicalActionRoute {
                    room: env.route.cluster_id.clone(),
                    app_id: env.route.app_id.clone(),
                    record_id: id.into(),
                }),
                max_results: 0,
                ..Default::default()
            };
            let edit = |provider: String, id: &str| InvokeRequest {
                invocation: tilo_actions::InvocationId::new(),
                target: RemoteActionTarget {
                    provider,
                    address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
                },
                argument: InvokeArgument::None,
                context: None,
                confirmation: None,
                revision: Some(format!("{id}-r1")),
                on_behalf_of: None,
                wait_ms: None,
                appearance: None,
            };
            for (id, _, stream) in &pages {
                let page = local_host_answer(host.query(&caller, canonical(id)))
                    .await
                    .unwrap();
                assert_eq!(page.providers.len(), 1, "{id} has its canonical publisher");
                let request = edit(page.providers[0].provider.clone(), id);
                local_host_answer(host.invoke(&caller, request.clone()))
                    .await
                    .unwrap_or_else(|error| {
                        panic!("{id} is editable from the other device: {error:?}")
                    });
                let delivered = tilo_test_env::within("the record's own delivery", stream.read())
                    .await
                    .unwrap();
                assert_eq!(
                    delivered.invocation_uuid,
                    *request.invocation.0.as_bytes(),
                    "{id}'s edit reaches {id}'s own stream"
                );
            }

            // The window closes page-b and keeps the others.
            let (_, closed, closed_stream) = pages.remove(1);
            drop((closed, closed_stream));
            let deadline = tilo_test_env::hang_deadline();
            while roots(&rig.state).contains("page-b") {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the dropped record's directory stays published"
                );
                tokio::task::yield_now().await;
            }
            assert_eq!(
                roots(&rig.state),
                ["page-a", "page-c"].map(String::from).into()
            );
            let gone = local_host_answer(host.query(&caller, canonical("page-b"))).await;
            assert!(
                gone.is_ok_and(|page| page.providers.is_empty()),
                "nothing publishes for the dropped record"
            );
            assert!(
                rig.state
                    .host_workers
                    .lock()
                    .unwrap()
                    .holder_of(&record("page-b"))
                    .is_none()
            );
            for id in ["page-a", "page-c"] {
                let page = local_host_answer(host.query(&caller, canonical(id)))
                    .await
                    .unwrap();
                assert_eq!(page.providers.len(), 1, "{id} keeps its publisher");
            }
            drop((pages, window));
            rig.stop().await;
        })
        .await;
}
