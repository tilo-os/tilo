use crate::BrokerError;
// An app the shell launched is the worker of its own routes: what it edits
// is admitted as its own actions, attributed to its person and to it.
// Included beside the local document report tests, whose rig it shares.

/// The launches this test process stands for, as the shell records the
/// apps it starts before releasing their launcher.
struct LaunchedHere {
    app: String,
    workspace: String,
    /// The caller the next connection's view is opened for, as the daemon opens a page's view.
    opened_for: Mutex<Option<tilo_actions::remote::Caller>>,
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
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id() && app == self.app && workspace == self.workspace
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        _window_instance: Option<u64>,
    ) -> Result<Option<crate::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, workspace).then_some(
            crate::services::AuthenticatedApplicationLaunch {
                continuation_instance: None,
                launch_ticket: None,
                windowless: false,
                opened_for: self.opened_for.lock().unwrap().clone(),
            },
        ))
    }
}

/// Connect as the launcher connects the app it starts: its marker and
/// handoff first, then the app's own wire session.
fn launched_app_connection(
    state: &Arc<AppBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    peer: u64,
    app: &str,
    workspace: &str,
) -> (
    runtime_collab::CollabClient,
    tokio::task::JoinHandle<Result<(), BrokerError>>,
    tokio::task::JoinHandle<()>,
) {
    launched_member_connection(state, directories, peer, (app, workspace), None)
}

/// [`launched_app_connection`] for a launch naming the workspace agent `member` the app acts as.
fn launched_member_connection(
    state: &Arc<AppBrokerState>,
    directories: &Rc<LocalActionDirectoryRegistry>,
    peer: u64,
    (app, workspace): (&str, &str),
    member: Option<&str>,
) -> (
    runtime_collab::CollabClient,
    tokio::task::JoinHandle<Result<(), BrokerError>>,
    tokio::task::JoinHandle<()>,
) {
    use std::io::Write as _;
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol": launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": app,
        "workspace_id": workspace,
        "member": member,
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
    let mut report = runtime_collab::LocalDocumentActionReport {
        ownership: Some(runtime_collab::committed_document::ConnectionSnapshot {
            root: env.cert.user_public_key,
            device: env.cert.device_public_key,
            agent: None,
            app_id: route.app_id.clone(),
            workspace: route.cluster_id.clone(),
            document: route.document_id.clone(),
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
    report.invocation = report.bound_invocation().unwrap();
    report
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
            services.workspace_grants = Some(Arc::new(LocalReportGrants(env.membership.clone())));
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                workspace: env.route.cluster_id.clone(),
                opened_for: Mutex::new(None),
            }));
            let rig = Rig::with_services(&env, services).await;
            let (app, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                APP_PEER,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let own = opened(&app, &env.route.channel_key()).await.unwrap();
            let worker = rig
                .state
                .host_workers
                .lock()
                .unwrap()
                .get(APP_PEER, &env.route)
                .cloned()
                .expect("the launched app is the worker of its own route");
            let registration = worker
                .worker_registration
                .expect("registered for the app its launcher vouched for");
            assert_eq!(registration.route, env.route);
            assert_eq!(registration.certificate, env.cert);

            own.publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(
                LocalDocumentDirectory {
                    kind: tilo_actions::OperationKind::Document,
                    read: false,
                    pin_dropped: Rc::new(std::cell::Cell::new(false)),
                    resolutions: Rc::new(std::cell::Cell::new(0)),
                },
            )))
            .await
            .unwrap();
            let person =
                tilo_action_policy::Grantee::person(person_id_for_root(env.cert.user_public_key));
            let grant = tilo_action_policy::Grant::new(
                "the-person-edits",
                person.clone(),
                env.membership.read().workspaces[0]
                    .policy
                    .node()
                    .app(env.route.app_id.clone()),
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                person,
            );
            env.membership.write().workspaces[0].policy.grant(grant);
            let report = edit_report(&env, &env.route);
            own.admit_local_document_action(&report)
                .await
                .expect("the launched app's edit is admitted");
            {
                let journal = rig.state.journal.lock().unwrap();
                let journal = journal.as_ref().unwrap().as_ref().unwrap();
                let key = InvocationKey {
                    workspace: env.route.cluster_id.clone(),
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

            // Another app's route in the same workspace, which this app declares
            // no read of: the connection may not join it, and no worker
            // speaks for it.
            let other = CrdtRoute::record("so.tilo.notes", env.route.cluster_id.clone(), "notes-1");
            let refused = opened(&app, &other.channel_key())
                .await
                .err()
                .expect("another app's route is not this app's to join");
            assert!(
                refused
                    .to_string()
                    .contains("declares no read of so.tilo.notes"),
                "{refused:?}"
            );
            assert!(
                rig.state
                    .host_workers
                    .lock()
                    .unwrap()
                    .holder_of(&other)
                    .is_none(),
                "no worker speaks for another app's route"
            );

            rig.state.remove_connection(APP_PEER);
            assert!(!rig.state.host_workers.lock().unwrap().holds(APP_PEER));
            drop((own, app));
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
            services.workspace_grants = Some(Arc::new(LocalReportGrants(env.membership.clone())));
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                workspace: env.route.cluster_id.clone(),
                opened_for: Mutex::new(None),
            }));
            let rig = Rig::with_services(&env, services).await;
            let person =
                tilo_action_policy::Grantee::person(person_id_for_root(env.cert.user_public_key));
            let grant = tilo_action_policy::Grant::new(
                "the-person-edits",
                person.clone(),
                env.membership.read().workspaces[0]
                    .policy
                    .node()
                    .app(env.route.app_id.clone()),
                tilo_action_policy::KindSet::ALL,
                tilo_action_policy::Answer::Allow,
                person,
            );
            env.membership.write().workspaces[0].policy.grant(grant);
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
                        opened(&app, &route(window, record).channel_key())
                            .await
                            .expect("a window joins every record it opens"),
                    );
                }
                channels[RECORDS - 1]
                    .publish_action_directory(crate::collab_app_broker::wire_answerer(Rc::new(
                        LocalDocumentDirectory {
                            kind: tilo_actions::OperationKind::Document,
                            read: false,
                            pin_dropped: Rc::new(std::cell::Cell::new(false)),
                            resolutions: Rc::new(std::cell::Cell::new(0)),
                        },
                    )))
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
                workspace: env.route.cluster_id.clone(),
                opened_for: Mutex::new(None),
            }));
            let rig = Rig::with_services(&env, services).await;
            let (window, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                WINDOW,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let record =
                |id: &str| CrdtRoute::record(&env.route.app_id, env.route.cluster_id.clone(), id);
            let mut pages = Vec::new();
            for id in ["page-a", "page-b", "page-c"] {
                let channel = opened(&window, &record(id).channel_key()).await.unwrap();
                channel
                    .publish_action_directory(super::signed_worker::directory_server(
                        &directory_state(&format!("{id}-r1")),
                    ))
                    .await
                    .unwrap();
                let stream = channel.subscribe_stable_invocations().await.unwrap();
                pages.push((id, channel, stream));
            }
            let roots = |state: &AppBrokerState| {
                state
                    .action_directory_roots()
                    .into_iter()
                    .filter(|root| state.connection_of(root.peer_id) == WINDOW)
                    .map(|root| root.document_id)
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
                .certified_local_action_provider(&caller, &evidence, false)
                .unwrap();
            let canonical = |id: &str| CatalogueQuery {
                canonical_route: Some(CanonicalActionRoute {
                    workspace: env.route.cluster_id.clone(),
                    app_id: env.route.app_id.clone(),
                    document_id: id.into(),
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
                signed: None,
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

/// A session that keeps its own journal still admits the app its launcher
/// started to that app's own route: the launch is the app's durable
/// admission, as a host-mounted worker's private descriptor is.
#[tokio::test(flavor = "current_thread")]
async fn a_journaled_session_admits_the_app_its_launcher_started() {
    tokio::task::LocalSet::new()
        .run_until(async {
            const APP_PEER: u64 = 907;
            let env = Environment::new();
            let mut services = env.membership.services();
            services.launcher = Some(Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                workspace: env.route.cluster_id.clone(),
                opened_for: Mutex::new(None),
            }));
            let journal = tempfile::tempdir().unwrap();
            let config = crate::NativePeerBrokerConfig {
                journal_root: journal.path().join("journal"),
                certificate_authority: env.authority.clone(),
            };
            let rig = Rig::with_own_journal(&env, services, &config).await;
            assert!(rig.state.durable_owner_required);
            let (app, _server, _client) = launched_app_connection(
                &rig.state,
                &rig.directories,
                APP_PEER,
                &env.route.app_id,
                &env.route.cluster_id,
            );
            let own = opened(&app, &env.route.channel_key())
                .await
                .expect("the launched app joins its own route in a journaled session");
            assert!(
                rig.state
                    .host_workers
                    .lock()
                    .unwrap()
                    .get(APP_PEER, &env.route)
                    .is_some_and(|worker| worker.worker_registration.is_some()),
                "the launched app is the worker of its own route"
            );

            // A record every app of the workspace opens is the workspace's,
            // not its owner's alone: any app of the workspace holds its route,
            // and the workspace's grants on the owner decide its changes.
            let shared = collab_types::WORKSPACE_DOCUMENTS
                .iter()
                .find(|record| record.app != env.route.app_id)
                .expect("a record every app of the workspace opens, kept by another app");
            let workspace_resource = CrdtRoute::record(
                shared.app,
                env.route.cluster_id.clone(),
                collab_types::workspace_document_id(shared.kind),
            );
            let kept = opened(&app, &workspace_resource.channel_key())
                .await
                .expect("an app of the workspace joins its workspace's record");

            // Another app's route in the same workspace, which this app
            // declares no read of: the launch vouches for nothing there.
            let other = CrdtRoute::record("so.tilo.notes", env.route.cluster_id.clone(), "notes-1");
            let refused = opened(&app, &other.channel_key())
                .await
                .err()
                .expect("another app's route is not this app's to join");
            assert!(refused.to_string().contains("so.tilo.notes"), "{refused:?}");
            drop((kept, own, app));
            rig.stop().await;
        })
        .await;
}

/// A session that keeps its own journal admits an app's declared read of
/// another app's record: the reader holds that route to read it live and
/// changes nothing there, so it needs no worker descriptor of the route and
/// becomes no worker of it. An app that declares no read is still refused.
#[tokio::test(flavor = "multi_thread")]
async fn a_journaled_session_admits_an_apps_declared_read_of_another_app() {
    static NOTES: &[u8] =
        b"{\"resources\":[{\"kind\":\"test.journal-notes.Note\",\"label\":\"Note\",\"version\":1}]}";
    static TODAY: &[u8] = b"{\"reads\":[\"test.journal-notes.Note\"],\"resources\":[{\"kind\":\"test.journal-today.Day\",\"label\":\"Day\",\"version\":1}]}";
    static TASKS: &[u8] =
        b"{\"resources\":[{\"kind\":\"test.journal-tasks.Task\",\"label\":\"Task\",\"version\":1}]}";
    collab_types::app_surface::register_package("test.journal-notes", NOTES);
    collab_types::app_surface::register_package("test.journal-today", TODAY);
    collab_types::app_surface::register_package("test.journal-tasks", TASKS);
    tokio::task::LocalSet::new()
        .run_until(async {
            const READER_PEER: u64 = 941;
            let mut env = Environment::new();
            // The records are a workspace's that the environment's own device keeps, and the
            // device's one reader decides a read of them, as a device serves its apps.
            let kept_here = tempfile::tempdir().unwrap();
            let host = tilo_workspace_service::workspaces::fixtures::test_host(
                kept_here.path(),
                &env.key,
                &env.cert,
            )
            .await;
            let kept = host.workspaces().create("Journal").await.unwrap().workspace;
            env.rehome(&kept);
            env.authority =
                CurrentHost::new(&env.route, &env.cert, Some(env.origin.capture().unwrap()));
            let notes = CrdtRoute::record(
                "test.journal-notes",
                env.route.cluster_id.clone(),
                "notes-1",
            );
            for (app, reads) in [("test.journal-today", true), ("test.journal-tasks", false)] {
                let mut services = env.membership.services();
                services.launcher = Some(Arc::new(LaunchedHere {
                    app: app.to_owned(),
                    workspace: env.route.cluster_id.clone(),
                    opened_for: Mutex::new(None),
                }));
                let journal = tempfile::tempdir().unwrap();
                let config = crate::NativePeerBrokerConfig {
                    journal_root: journal.path().join("journal"),
                    certificate_authority: env.authority.clone(),
                };
                let rig = Rig::with_own_journal(&env, services, &config).await;
                assert!(rig.state.durable_owner_required);
                rig.state.serve_records(
                    tilo_workspace_service::workspaces::RecordReads::with_apps(
                        host.workspaces(),
                        &rig.state,
                        None,
                    ),
                );
                let (launched, _server, _client) = launched_app_connection(
                    &rig.state,
                    &rig.directories,
                    READER_PEER,
                    app,
                    &env.route.cluster_id,
                );
                let joined = opened(&launched, &notes.channel_key()).await;
                if reads {
                    let reading = joined.expect("a declared read joins in a journaled session");
                    assert!(
                        rig.state
                            .host_workers
                            .lock()
                            .unwrap()
                            .get(READER_PEER, &notes)
                            .is_none(),
                        "a reader is no worker of the route it reads"
                    );
                    drop(reading);
                } else {
                    let refused = joined.err().expect("an undeclared read joins nothing");
                    assert!(
                        refused.to_string().contains("declares no read of"),
                        "{refused:?}"
                    );
                }
                rig.state.remove_connection(READER_PEER);
                drop(launched);
                rig.stop().await;
            }
            host.shutdown().await;
        })
        .await;
}

/// A press in a view the device opened for a caller of its dispatch acts as that caller, never as
/// the device's own person: a member the workspace lets act in the app is admitted as themselves,
/// and one it does not is refused, although the device's person may do it.
#[tokio::test(flavor = "current_thread")]
async fn a_press_in_a_view_opened_for_a_caller_acts_as_that_caller() {
    tokio::task::LocalSet::new()
        .run_until(async {
            use crate::test_support::{Grants as _, KindSet};
            use tilo_actions::remote::Caller;
            let env = Environment::new();
            let workspace = env.route.cluster_id.clone();
            let editor = person_id_for_root([7; 32]);
            let stranger = person_id_for_root([5; 32]);
            env.membership.write().workspaces[0].allow_app(
                tilo_action_policy::Grantee::person(editor.clone()),
                &env.route.app_id,
                KindSet::ALL,
            );
            let launcher = Arc::new(LaunchedHere {
                app: env.route.app_id.clone(),
                workspace: workspace.clone(),
                opened_for: Mutex::new(None),
            });
            let mut services = env.membership.services();
            services.launcher = Some(launcher.clone());
            let rig = Rig::with_services(&env, services).await;
            let view_for = |peer: u64, person: &str| {
                *launcher.opened_for.lock().unwrap() =
                    Some(Caller::local_person(person).in_workspace(workspace.clone()));
                launched_app_connection(
                    &rig.state,
                    &rig.directories,
                    peer,
                    &env.route.app_id,
                    &workspace,
                )
            };

            // A view opened for someone the workspace does not let act in the app: the press is
            // refused as the workspace refuses them anything, without saying the action exists,
            // and nothing reaches the app.
            let (stranger_view, _server, _client) = view_for(906, &stranger);
            let stranger_view = opened(&stranger_view, &env.route.channel_key())
                .await
                .unwrap();
            let refused = invoke(&stranger_view, id(0xd001)).await.unwrap_err();
            assert!(refused.contains("UnknownAction"), "{refused}");
            no_delivery(&rig).await;

            // A view opened for the editor: the press is admitted, as the editor.
            let (editor_view, _server, _client) = view_for(907, &editor);
            let editor_view = opened(&editor_view, &env.route.channel_key())
                .await
                .unwrap();
            invoke(&editor_view, id(0xd002)).await.unwrap();
            let delivered = rig.stream.as_ref().unwrap().read().await.unwrap();
            assert_eq!(
                delivered.actor,
                tilo_actions::Actor::Member(tilo_actions::MemberId::person(editor.clone())),
                "the press is the editor's, not the device's person's"
            );

            // What would need the editor's confirmation waits for the editor, legibly: the
            // dispatch keeps it as the editor's asked record, which nobody here answers.
            env.membership.write().workspaces[0].answer(
                tilo_action_policy::Grantee::person(editor.clone()),
                &tilo_action_policy::TreePath::root().app(&env.route.app_id),
                KindSet::ALL,
                tilo_action_policy::Answer::Ask,
            );
            let refused = invoke(&editor_view, id(0xd003)).await.unwrap_err();
            assert!(refused.contains("ConfirmationRequired"), "{refused}");
            no_delivery(&rig).await;
            drop((stranger_view, editor_view));
            rig.stop().await;
        })
        .await;
}
