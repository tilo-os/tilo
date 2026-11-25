// Native launcher-authenticated dispatch for informational progress tests.
struct ProgressFixtureLaunch {
    app: String,
    workspace: String,
}
impl crate::apps::services::LauncherServices for ProgressFixtureLaunch {
    fn applications(&self) -> Vec<crate::apps::services::LaunchableApplication> {
        Vec::new()
    }
    fn launch(
        &self,
        _: &str,
        _: &tilo_desktop_model::Workspace,
        _: Option<&str>,
        _: &crate::apps::services::LaunchPlace,
    ) -> Result<(), String> {
        Err("fixture never launches apps".into())
    }
    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        pid == std::process::id() && app == self.app && workspace == self.workspace
    }
}
fn progress_descriptors() -> Vec<runtime_collab::ExportedAction> {
    let directory = signed_worker::directory_state("reported-check-r1");
    let mut check = directory.borrow().row().descriptor.unwrap();
    check.action = runtime_collab::approved_checks::ACTION_ID.into();
    check.operation_kind = tilo_actions::OperationKind::Effect;
    check.semantic_identity = Some(runtime_collab::ExportedResourceSemanticIdentity {
        kind: check.resource_kind.clone(),
        key: "check".into(),
    });
    check.contract = tilo_actions::ActionContract {
        kind: tilo_actions::ActionKind::Undoable,
        result: tilo_actions::ResultSpec::Job,
        execution: tilo_actions::ExecutionMode::Background { cancellable: true },
        // Its one declared step: the declared probe's run.
        effects: vec![tilo_actions::ActionEffect::new(
            DECLARED_PROBE,
            "checks",
            "run",
        )],
        ..Default::default()
    };
    vec![check]
}
async fn progress_native_connection(
    workspace: &signed_worker::OriginalWorkspace,
    providers: crate::workspaces::providers::Providers,
) -> Result<
    (
        Arc<AppBrokerState>,
        signed_worker::Connection,
        Rc<LocalActionDirectoryRegistry>,
    ),
    String,
> {
    // The replacement broker reuses the workspace's journal, which has one writer.
    drop(
        workspace
            .state
            .journal
            .lock()
            .unwrap()
            .as_mut()
            .unwrap()
            .take(),
    );
    let (state, bridge) = AppBrokerState::test_state(
        Some(workspace.membership.authority()),
        ProcessServices {
            launcher: Some(Arc::new(ProgressFixtureLaunch {
                app: workspace.route.app_id.clone(),
                workspace: workspace.route.cluster_id.clone(),
            })),
            providers: Some(providers),
            ..workspace.membership.services()
        },
    );
    assert!(
        state
            .certificate_authority
            .set(workspace.authority.clone())
            .is_ok()
    );
    let directories = Rc::new(LocalActionDirectoryRegistry::new(bridge));
    let (server, mut client) = std::os::unix::net::UnixStream::pair().unwrap();
    std::io::Write::write_all(&mut client, &launcher::services::COLLAB_LAUNCH_MAGIC).unwrap();
    let handoff = serde_json::to_vec(&serde_json::json!({
        "protocol":launcher::services::COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id":workspace.route.app_id,"workspace_id":workspace.route.cluster_id,
    }))
    .unwrap();
    std::io::Write::write_all(
        &mut client,
        &launcher::services::COLLAB_LAUNCH_FRAMING
            .encode(&handoff)
            .unwrap(),
    )
    .unwrap();
    let (rpc, server, client) = signed_worker::rpc_pair(
        state.clone(),
        997,
        server,
        client,
        None,
        directories.clone(),
    );
    let handle = opened(&rpc, &workspace.route.channel_key())
        .await
        .map_err(|error| error.to_string())?;
    Ok((
        state,
        signed_worker::Connection {
            handle,
            server,
            client,
        },
        directories,
    ))
}
/// The host's own person on their own device, as the shell's certified
/// session presents them.
fn person(workspace: &signed_worker::OriginalWorkspace) -> (tilo_actions::remote::Caller, Vec<u8>) {
    (
        tilo_actions::remote::Caller::authenticated(
            workspace.cert.user_public_key,
            workspace.cert.device_public_key,
        )
        .in_workspace(&workspace.route.cluster_id)
        .in_new_execution(),
        signed_worker::presentation(&workspace.route.cluster_id, &workspace.cert, &workspace.key),
    )
}

/// Drive `pending` while the broker's turn resolves directories, as its
/// accept loop does.
#[allow(
    clippy::panic,
    reason = "test code included into the broker module: clippy does not see the cfg(test) on its include!, so it reads the test's checks as production code"
)]
async fn on_broker_turn<T>(
    (state, directories): (&Arc<AppBrokerState>, &Rc<LocalActionDirectoryRegistry>),
    mut pending: tilo_actions::remote::Pending<T>,
) -> T {
    tilo_test_env::within("the broker answers the person", async {
        loop {
            state.take_turn(directories);
            match pending.poll_ready() {
                tilo_actions::remote::PendingPoll::Ready(answer) => return answer,
                tilo_actions::remote::PendingPoll::Waiting => tokio::task::yield_now().await,
                tilo_actions::remote::PendingPoll::Abandoned => panic!("the request was dropped"),
            }
        }
    })
    .await
}

/// The person's certified session with the host: what they ask and
/// cancel there is theirs.
struct PersonSession {
    caller: tilo_actions::remote::Caller,
    provider: Arc<dyn tilo_actions::remote::ActionProvider + Send + Sync>,
}

impl PersonSession {
    fn open(state: &Arc<AppBrokerState>, workspace: &signed_worker::OriginalWorkspace) -> Self {
        let (caller, evidence) = person(workspace);
        let provider = state
            .certified_local_action_provider(&caller, &evidence, false)
            .unwrap();
        Self { caller, provider }
    }

    /// Ask `peer`'s approved check on `resource`.
    async fn runs_check(
        &self,
        broker: (&Arc<AppBrokerState>, &Rc<LocalActionDirectoryRegistry>),
        peer: u64,
        resource: &str,
    ) -> tilo_actions::remote::InvocationReceipt {
        let request = signed_worker::invoke_request(
            &peer.to_string(),
            resource,
            runtime_collab::approved_checks::ACTION_ID,
            Some(serde_json::json!({"check_id": "regression"})),
        );
        let receipt = on_broker_turn(broker, self.provider.invoke(&self.caller, request))
            .await
            .unwrap();
        assert!(
            matches!(
                receipt.status,
                tilo_actions::remote::ReceiptStatus::Accepted
            ),
            "{receipt:?}"
        );
        receipt
    }

    /// Cancel the check `invocation` they asked.
    async fn cancels(
        &self,
        broker: (&Arc<AppBrokerState>, &Rc<LocalActionDirectoryRegistry>),
        invocation: tilo_actions::InvocationId,
    ) {
        on_broker_turn(broker, self.provider.cancel(&self.caller, &invocation))
            .await
            .unwrap();
    }
}

async fn progress_parent(
    (state, directories): (&Arc<AppBrokerState>, &Rc<LocalActionDirectoryRegistry>),
    workspace: &signed_worker::OriginalWorkspace,
    connection: &signed_worker::Connection,
    descriptors: &[runtime_collab::ExportedAction],
    peer: u64,
) -> (
    runtime_collab::InvocationStream,
    runtime_collab::Invocation,
    PersonSession,
) {
    connection
        .handle
        .publish_actions(descriptors)
        .await
        .unwrap();
    let stream = connection
        .handle
        .subscribe_stable_invocations()
        .await
        .unwrap();
    let session = PersonSession::open(state, workspace);
    let accepted = session
        .runs_check((state, directories), peer, "workspace:atlas")
        .await;
    let parent = stream.read().await.unwrap();
    assert_eq!(parent.invocation_uuid, *accepted.invocation.0.as_bytes());
    (stream, parent, session)
}

/// A device provider an app's action declares as its step, as the device's approved checks are:
/// it keeps what the dispatch told it the call is a step of, says how far it got, and ends when the
/// test ends it.
/// What a provider was told a call serves: the app, workspace, record and action of the
/// delivered invocation.
type Told = (String, String, String, tilo_actions::ActionAddress);

#[derive(Default)]
struct ServedProbe {
    seen: Mutex<Vec<Option<Told>>>,
    ends: Mutex<Vec<tilo_actions::remote::Completer<tilo_actions::remote::Outcome>>>,
}

const DECLARED_PROBE: &str = "tilo.declared-probe";
const UNDECLARED_PROBE: &str = "tilo.undeclared-probe";

fn probe_rows(provider: &str) -> Vec<tilo_actions::api::PlacedAction> {
    let descriptor = tilo_actions::ActionDescriptor {
        did: None,
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        address: tilo_actions::ActionAddress::new("checks", "run"),
        resource_kind: provider.to_owned(),
        resource_label: "Checks".to_owned(),
        action_label: "Run".to_owned(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        locus: tilo_actions::ExecutionLocus::ClientLocal,
        kind: tilo_actions::OperationKind::Effect,
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Undoable,
            execution: tilo_actions::ExecutionMode::Background { cancellable: true },
            ..Default::default()
        },
        input: tilo_actions::api::InputSpec::None,
        params: vec![tilo_actions::params::ActionParam::required(
            "check_id",
            tilo_actions::params::ParamType::Text,
            "The check",
        )],
        view: None,
    };
    tilo_actions::remote::admitted::top_level(vec![descriptor])
}

impl ServedProbe {
    fn start(
        &self,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        self.seen
            .lock()
            .unwrap()
            .push(admitted.serving.as_ref().map(|serving| {
                (
                    serving.app().to_owned(),
                    serving.workspace().to_owned(),
                    serving.record().to_owned(),
                    serving.address().clone(),
                )
            }));
        let serving = admitted
            .serving
            .as_ref()
            .ok_or("no invocation this runs for")?;
        serving.recheck()?;
        (admitted.progress)("tests", Some(1), Some(2));
        let (end, outcome) = tilo_actions::remote::pending();
        self.ends.lock().unwrap().push(end);
        Ok(outcome)
    }
}

/// The probe as the provider of the step the action declares.
struct Served(Arc<ServedProbe>);
impl tilo_actions::remote::Admits for Served {
    const PROVIDER: &'static str = DECLARED_PROBE;
    const APP: &'static str = tilo_app_id::apps::SETTINGS;
    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        probe_rows(DECLARED_PROBE)
    }
    fn start(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::remote::InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        self.0.start(admitted)
    }
}

/// The same probe as a provider of this device the action declares no step of.
struct Other(Arc<ServedProbe>);
impl tilo_actions::remote::Admits for Other {
    const PROVIDER: &'static str = UNDECLARED_PROBE;
    const APP: &'static str = tilo_app_id::apps::SETTINGS;
    fn rows(&self) -> Vec<tilo_actions::api::PlacedAction> {
        probe_rows(UNDECLARED_PROBE)
    }
    fn start(
        &self,
        _: &tilo_actions::remote::Caller,
        _: &tilo_actions::remote::InvokeRequest,
        admitted: &tilo_actions::remote::Admitted,
    ) -> Result<tilo_actions::remote::Pending<tilo_actions::remote::Outcome>, String> {
        self.0.start(admitted)
    }
}

/// What the app's connection hears back when it calls `provider`'s run as the work of `parent`.
async fn app_calls(
    connection: &signed_worker::Connection,
    provider: &str,
    parent: tilo_actions::InvocationId,
) -> (
    tilo_actions::InvocationId,
    tilo_actions::remote::RemoteActionMessage,
) {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    let invocation = tilo_actions::InvocationId::new();
    let request = tilo_actions::remote::InvokeRequest {
        invocation,
        target: tilo_actions::remote::RemoteActionTarget {
            provider: provider.to_owned(),
            address: tilo_actions::ActionAddress::new("checks", "run"),
        },
        argument: tilo_actions::remote::InvokeArgument::Json(
            serde_json::json!({"check_id": "regression"})
                .as_object()
                .cloned()
                .unwrap(),
        ),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: Some(parent),
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let frame = RemoteActionFrame::new(RemoteActionMessage::Invoke(request))
        .encode()
        .unwrap();
    let answer = RemoteActionFrame::decode(&connection.handle.remote_action(&frame).await.unwrap())
        .unwrap()
        .message;
    (invocation, answer)
}

/// What the app's connection reads of its call `invocation` once it moved on.
async fn app_observes(
    connection: &signed_worker::Connection,
    invocation: tilo_actions::InvocationId,
) -> tilo_actions::remote::RemoteActionMessage {
    use tilo_actions::remote::{RemoteActionFrame, RemoteActionMessage};
    let frame = RemoteActionFrame::new(RemoteActionMessage::Observe {
        correlation: 1,
        invocation,
        wait_ms: None,
        seen: None,
    })
    .encode()
    .unwrap();
    RemoteActionFrame::decode(&connection.handle.remote_action(&frame).await.unwrap())
        .unwrap()
        .message
}

fn status_of(
    answer: &tilo_actions::remote::RemoteActionMessage,
) -> Option<&tilo_actions::remote::ReceiptStatus> {
    match answer {
        tilo_actions::remote::RemoteActionMessage::Receipt(receipt) => Some(&receipt.status),
        _ => None,
    }
}

/// An app's call made inside an invocation delivered to its connection, to an action that
/// invocation's action declares as its step, is covered by the yes the invocation was given: its
/// provider is told exactly that invocation, as delivered, and nobody is asked again; it is
/// followed through the progress it reports to its end. A call to an action the parent declares
/// no step of is decided as any call continuing it, its provider told of no invocation, and
/// naming an invocation not delivered to the connection is refused, a cancelled one too.
#[tokio::test(flavor = "current_thread")]
#[allow(
    clippy::too_many_lines,
    reason = "one app connection's calls, from its delivered invocation to its cancellation"
)]
async fn an_apps_call_for_a_delivered_invocation_reaches_the_device_as_that_invocations_work() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let workspace = signed_worker::OriginalWorkspace::new(
                tilo_user_identity::DeviceCapabilities::primary(),
                None,
                None,
            );
            let probe = Arc::new(ServedProbe::default());
            let providers = crate::workspaces::providers::Providers::default();
            providers.provide(
                DECLARED_PROBE,
                tilo_app_id::apps::SETTINGS,
                crate::workspaces::providers::Reach::ThisDevice,
                Arc::new(Served(Arc::clone(&probe))),
            );
            providers.provide(
                UNDECLARED_PROBE,
                tilo_app_id::apps::SETTINGS,
                crate::workspaces::providers::Reach::ThisDevice,
                Arc::new(Other(Arc::clone(&probe))),
            );
            let descriptors = progress_descriptors();
            let (state, connection, directories) =
                progress_native_connection(&workspace, providers)
                    .await
                    .unwrap();
            let broker = (&state, &directories);
            let (_invocations, parent, session) =
                progress_parent(broker, &workspace, &connection, &descriptors, 997).await;
            let parent = tilo_actions::InvocationId(uuid::Uuid::from_bytes(parent.invocation_uuid));

            // The work of the delivered invocation runs, told that invocation, and nobody is
            // asked again.
            let (call, answer) = app_calls(&connection, DECLARED_PROBE, parent).await;
            assert!(
                matches!(status_of(&answer).expect("the call is answered with its receipt"), tilo_actions::remote::ReceiptStatus::Accepted),
                "{answer:?}"
            );
            let route = &workspace.route;
            assert_eq!(
                probe.seen.lock().unwrap().as_slice(),
                [Some((
                    route.app_id.clone(),
                    route.cluster_id.clone(),
                    route.document_id.clone(),
                    tilo_actions::ActionAddress::new(
                        "workspace:atlas",
                        runtime_collab::approved_checks::ACTION_ID
                    ),
                ))],
                "the provider is told the delivered invocation, as delivered"
            );
            // How far it got is read by whoever follows the call.
            let observed = app_observes(&connection, call).await;
            let tilo_actions::remote::RemoteActionMessage::Update {
                state: Some(followed),
                ..
            } = observed
            else {
                panic!("the running call is followed: {observed:?}");
            };
            assert!(
                matches!(
                    &followed.update,
                    tilo_actions::InvocationUpdate::Progress { phase, completed: Some(1), total: Some(2), .. }
                        if phase == "tests"
                ),
                "{followed:?}"
            );
            probe.ends.lock().unwrap().pop().unwrap().complete(Ok(tilo_actions::ActionOutput {
                value: Some(serde_json::json!({"outcome": "exited"})),
                ..Default::default()
            }));
            let ended = tilo_test_env::within("the work ends", async {
                loop {
                    if let tilo_actions::remote::RemoteActionMessage::Update {
                        state: Some(followed),
                        ..
                    } = app_observes(&connection, call).await
                        && followed.update.is_terminal()
                    {
                        return followed.update;
                    }
                    tokio::task::yield_now().await;
                }
            })
            .await;
            assert!(
                matches!(ended, tilo_actions::InvocationUpdate::Succeeded { .. }),
                "{ended:?}"
            );

            // A step the action does not declare is no part of what its yes covered: it is
            // decided as any call continuing the invocation, and its provider is told of no
            // invocation it is a step of.
            let (_, answer) = app_calls(&connection, UNDECLARED_PROBE, parent).await;
            assert!(
                !matches!(
                    status_of(&answer).expect("the call is answered with its receipt"),
                    tilo_actions::remote::ReceiptStatus::Accepted
                        | tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{answer:?}"
            );
            assert_eq!(
                probe.seen.lock().unwrap().last(),
                Some(&None),
                "an undeclared step's provider is never told the invocation"
            );
            let seen = probe.seen.lock().unwrap().len();

            // An invocation this connection was not delivered is no invocation to work for.
            let (_, answer) =
                app_calls(&connection, DECLARED_PROBE, tilo_actions::InvocationId::new()).await;
            assert!(
                matches!(
                    status_of(&answer).expect("the call is answered with its receipt"),
                    tilo_actions::remote::ReceiptStatus::Rejected {
                        error: tilo_actions::remote::ActionCallError::NotPermitted
                    }
                ),
                "{answer:?}"
            );
            assert_eq!(
                probe.seen.lock().unwrap().len(),
                seen,
                "the provider is never handed it"
            );

            // Once the person cancels the invocation, nothing is its step any more: no call
            // starts as its work.
            session.cancels(broker, parent).await;
            let (_, answer) = app_calls(&connection, DECLARED_PROBE, parent).await;
            assert!(
                !matches!(
                    status_of(&answer).expect("the call is answered with its receipt"),
                    tilo_actions::remote::ReceiptStatus::Accepted
                        | tilo_actions::remote::ReceiptStatus::Completed { .. }
                ),
                "{answer:?}"
            );
            assert!(
                probe.seen.lock().unwrap()[seen..].iter().all(Option::is_none),
                "a cancelled invocation's step is never handed on as its work"
            );
        })
        .await;
}
