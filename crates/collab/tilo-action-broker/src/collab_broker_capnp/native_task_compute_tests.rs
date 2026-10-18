//! Protected managed subscription tests; no application or provider is launched.
use super::*;

#[derive(Debug)]
pub(super) struct EnrolledActor {
    certificate: tilo_user_identity::DeviceCert,
    delegation: tilo_workspace_join::delegation::Delegation,
    key: SigningKey,
}
impl crate::services::ManagedApplicationIdentity for EnrolledActor {
    fn identity(
        &self,
    ) -> Result<
        (
            tilo_user_identity::DeviceCert,
            tilo_workspace_join::delegation::Delegation,
            [u8; 64],
        ),
        String,
    > {
        Ok((
            self.certificate.clone(),
            self.delegation.clone(),
            self.key
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &room().to_string(),
                ))
                .to_bytes(),
        ))
    }
}
pub(super) fn enrolled_actor(fixture: &Fixture) -> Arc<EnrolledActor> {
    enrolled_native_actor(&fixture.native)
}

// Deterministic signing custody belongs only to this native actor fixture.
pub(super) fn enrolled_actor_key() -> SigningKey {
    SigningKey::from_bytes(&[93; 32])
}

pub(super) fn enrolled_native_actor(native: &Native) -> Arc<EnrolledActor> {
    use tilo_workspace_join::delegation::{Delegate, DelegateScope, Delegation};
    let root = SigningKey::from_bytes(&[91; 32]);
    let key = enrolled_actor_key();
    let mut certificate = tilo_joined_room::testing::certificate(&root, &key, "Scribe");
    certificate.capabilities = tilo_user_identity::DeviceCapabilities {
        can_own_workspaces: false,
        can_admit_devices: false,
        can_act_as_host: false,
        can_read_all_workspaces: false,
        foreign: false,
        can_administer_devices: false,
    };
    tilo_user_identity::sign_device_cert(&mut certificate, &root).unwrap();
    let now = tilo_clock::unix_now_ns();
    let delegation = Delegation::sign(
        &root,
        certificate.device_public_key,
        Delegate::Agent {
            agent_id: "scribe".into(),
        },
        "Scribe",
        [10, 20, 30],
        DelegateScope {
            rooms: vec![room().to_string()],
            apps: vec!["so.tilo.agent".into()],
        },
        now,
        now.checked_add(60 * 60 * 1_000_000_000).unwrap(),
    )
    .unwrap();
    native
        .owner
        .admit_managed_agent(
            room(),
            certificate.clone(),
            delegation.clone(),
            &native.certificate,
            &native.key,
            std::time::SystemTime::now(),
        )
        .unwrap();
    Arc::new(EnrolledActor {
        certificate,
        delegation,
        key,
    })
}

fn managed_channel(f: &Fixture, peer: u64) -> (channel_handle::Client, tokio::net::UnixStream) {
    let route = CrdtRoute::record("so.tilo.agent", room().to_string(), "main");
    let mut admitted = f
        .state
        .verify_action_caller(&room().to_string(), &f.signed_presentation())
        .unwrap()
        .admitted;
    let (socket, remote) = tokio::net::UnixStream::pair().unwrap();
    let mut local = native_media::LocalAppConnection::attested(
        "so.tilo.agent".into(),
        room().to_string(),
        native_media::PeerProcess::from_socket(&socket).unwrap(),
        crate::services::AuthenticatedApplicationLaunch {
            managed_task_agent: Some("scribe".into()),
            managed_identity: Some(enrolled_actor(f)),
            creation: None,
            continuation_instance: None,
            launch_ticket: None,
        },
    )
    .unwrap();
    Arc::get_mut(&mut local).unwrap().protected_model =
        Some(launcher::registration_gate::ProtectedModelLaunch {
            app: "so.tilo.agent".into(),
            room: room().to_string(),
            agent: "scribe".into(),
            launch: uuid::Uuid::new_v4().to_string(),
        });
    f.state
        .admit_managed_task_identity(&local, &route, &mut admitted)
        .unwrap();
    assert_ne!(
        admitted.certificate.as_ref().unwrap(),
        &f.native.certificate
    );
    let channel = capnp_rpc::new_client(CapnpChannelHandle {
        person_chat_views: RefCell::default(),
        committed_read: Rc::new(RefCell::new(committed_document::Slot::default())),
        state: f.state.clone(),
        directories: f.directories.clone(),
        channel_id: route.channel_key(),
        peer_id: peer,
        verified_identity: admitted,
        record_kind: None,
        lease: Some(Rc::new(ConnectionLease {
            local_media: Some(local),
            state: f.state.clone(),
            directories: f.directories.clone(),
            peer_id: peer,
        })),
    });
    (channel, remote)
}

#[tokio::test(flavor = "current_thread")]
async fn only_protected_subscription_is_live_and_disconnect_retains_only_relaunch_candidate() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let registry = Arc::new(crate::services::NativeTaskExecutorRegistry::default());
            let f = Fixture::with_executor_registry(None, Some(registry.clone()));
            let unmanaged = f.channel(77, true);
            let _unmanaged_stream = unmanaged
                .subscribe_task_runs_request()
                .send()
                .promise
                .await
                .unwrap();
            assert!(registry.candidates(&room().to_string()).unwrap().is_empty());
            let (managed, _socket) = managed_channel(&f, 79);
            assert!(registry.candidates(&room().to_string()).unwrap().is_empty());
            let _stream = managed
                .subscribe_task_runs_request()
                .send()
                .promise
                .await
                .unwrap();
            let executor = registry
                .executor_for_peer(79, &room().to_string(), "scribe")
                .unwrap();
            assert_ne!(executor.certificate, f.native.certificate);
            assert!(registry.is_current(&executor));
            assert!(
                f.native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .controls[&f.task]
                    .active_run
                    .is_none(),
                "subscription is not consent"
            );
            f.state.disconnect_native_task_stream(79);
            assert!(
                registry
                    .live_executor(&room().to_string(), "scribe")
                    .is_none()
            );
            assert_eq!(registry.candidates(&room().to_string()).unwrap().len(), 1);
            let presentation = f
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
            assert_eq!(
                presentation.len(),
                1,
                "disconnect retains relaunch selection"
            );
            assert!(!presentation[0].current_certified_executor);
            assert!(presentation[0].model_ready);
            assert!(!registry.is_current(&executor));
            let _stream = managed
                .subscribe_task_runs_request()
                .send()
                .promise
                .await
                .unwrap();
            assert!(
                registry
                    .live_executor(&room().to_string(), "scribe")
                    .is_some()
            );
            f.remove_agent();
            assert!(
                registry
                    .live_executor(&room().to_string(), "scribe")
                    .is_none()
            );
            assert_eq!(registry.candidates(&room().to_string()).unwrap().len(), 1);
            drop(f);
        })
        .await;
}

pub(super) async fn invoke_hook(
    f: &Fixture,
    resource: &str,
    action: &str,
    argument: Option<serde_json::Value>,
) -> tilo_actions::InvocationUpdate {
    f.state.refresh_workspace_membership_services().unwrap();
    let channel = f.app_channel(100, "so.tilo.chat", false);
    // The native owner publishes an eager service surface, not an application
    // ActionDirectory capability. Discover it through the authenticated catalogue.
    let reply = channel
        .action_catalogue_request()
        .send()
        .promise
        .await
        .unwrap();
    let catalogue: serde_json::Value = serde_json::from_str(
        reply
            .get()
            .unwrap()
            .get_catalogue_json()
            .unwrap()
            .to_str()
            .unwrap(),
    )
    .unwrap();
    let workspace = room().to_string();
    let surface = catalogue["surfaces"]
        .as_array()
        .unwrap()
        .iter()
        .find(|surface| {
            surface["app_id"] == "so.tilo.desktop"
                && surface["cluster_id"] == workspace
                && surface["record_id"] == "membership"
                && surface["live"] == true
        })
        .expect("native owner must publish its live workspace service");
    assert!(
        surface["actions"]
            .as_array()
            .unwrap()
            .iter()
            .any(|offered| { offered["resource"] == resource && offered["action"] == action }),
        "native owner must publish {resource}#{action}: {surface}"
    );
    let peer = surface["peer_id"].as_u64().unwrap();
    assert_eq!(
        peer,
        f.state.workspace_membership_peers.lock().unwrap()[&workspace]
    );
    let mut invoke = channel.invoke_published_action_request();
    {
        let mut p = invoke.get();
        p.set_peer_id(peer);
        p.set_resource(resource);
        p.set_action(action);
        p.set_has_argument(argument.is_some());
        if let Some(argument) = argument {
            p.set_argument(argument.to_string());
        }
        p.set_confirmation_json("{}");
    }
    let accepted = invoke.send().promise.await.unwrap();
    let accepted = accepted.get().unwrap();
    let first: tilo_actions::InvocationUpdate =
        serde_json::from_str(accepted.get_update_json().unwrap().to_str().unwrap()).unwrap();
    let invocation = first.invocation();
    let host = f.state.invocation_host().unwrap();
    let deadline = tilo_test_env::hang_deadline();
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "hook did not finish: {action}"
        );
        let mut status = channel.published_invocation_observation_request();
        status.get().set_host(host.as_bytes());
        status.get().set_invocation(invocation.0.as_bytes());
        let response = status.send().promise.await.unwrap();
        let response = response.get().unwrap().get_observation().unwrap();
        assert!(response.get_found(), "accepted hook must retain its status");
        let update: tilo_actions::InvocationUpdate =
            serde_json::from_str(response.get_update_json().unwrap().to_str().unwrap()).unwrap();
        if update.is_terminal() {
            return update;
        }
        tokio::task::yield_now().await;
    }
}

// Supply only host-owned discovery metadata. Authentication, publication and
// invocation still run through the real signed-person Cap'n Proto fixture.
struct ComputeDiscovery {
    native: Arc<Native>,
    configured: Option<String>,
    registrations: Vec<crate::services::NativeTaskComputeRegistration>,
}
impl ComputeDiscovery {
    fn assert_caller(&self, requested: &str, caller: &tilo_user_identity::DeviceCert) {
        assert_eq!(requested, room().to_string());
        assert_eq!(caller, &self.native.certificate);
    }
}
impl WorkspaceMemberships for ComputeDiscovery {
    fn requires_room(&self, requested: &str) -> bool {
        WorkspaceMemberships::requires_room(self.native.as_ref(), requested)
    }
    fn health(&self) -> Result<(), String> {
        self.native.owner.health()
    }
    fn task_snapshot(&self, requested: &str) -> Result<WorkspaceTaskSnapshot, String> {
        self.native.task_snapshot(requested)
    }
    fn execute(&self, request: OwnerCommitRequest) -> CommitFuture {
        self.native.commit(request)
    }
    fn task_compute(
        &self,
        requested: &str,
        caller: &tilo_user_identity::DeviceCert,
    ) -> Result<Option<Vec<crate::services::NativeTaskComputeRegistration>>, String> {
        self.assert_caller(requested, caller);
        Ok(Some(self.registrations.clone()))
    }
    fn task_compute_configuration(
        &self,
        requested: &str,
        caller: &tilo_user_identity::DeviceCert,
    ) -> Result<Option<String>, String> {
        self.assert_caller(requested, caller);
        Ok(self.configured.clone())
    }
}

fn discovery_fixture(
    configured: Option<String>,
    registrations: Vec<crate::services::NativeTaskComputeRegistration>,
) -> Fixture {
    let mut f = Fixture::new();
    let config = crate::peer_actions::NativePeerBrokerConfig {
        journal_root: f.directory.path().join("compute-journal"),
        certificate_authority: f.native.clone(),
    };
    let (state, rx) = CapnpBrokerState::new_configured(
        &f.fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::with_optional_owner_commit(None, f.native.clone()),
        Some(f.native.clone()),
        None,
        Some(&config),
        HostServices {
            workspace_memberships: Some(Arc::new(ComputeDiscovery {
                native: f.native.clone(),
                configured,
                registrations,
            })),
            ..Default::default()
        },
    );
    f.state = state;
    f.directories = Rc::new(LocalActionDirectoryRegistry::new(rx));
    f
}

#[tokio::test(flavor = "current_thread")]
async fn published_compute_read_has_exact_setup_states_and_no_configuration_side_effects() {
    tokio::task::LocalSet::new().run_until(async {
        for (configured, registrations, status, workers) in [
            (None, Vec::new(), "needsConfiguration", serde_json::json!([])),
            (Some("Configured model".into()), Vec::new(), "needsExecutorSetup", serde_json::json!([])),
            (Some("Configured model".into()), vec![crate::services::NativeTaskComputeRegistration {
                agent_id: "scribe".into(), workspace: room().to_string(), profile: "Configured model".into(),
                current_certified_executor: false, model_ready: true, permitted: true,
            }], "needsExecutorSetup", serde_json::json!([{
                "agent_id":"scribe", "workspace":room().to_string(), "profile":"Configured model",
                "eligible":false, "configured":true, "registered":false,
            }])),
            (Some("Configured model".into()), vec![crate::services::NativeTaskComputeRegistration {
                agent_id: "scribe".into(), workspace: room().to_string(), profile: "Configured model".into(),
                current_certified_executor: true, model_ready: true, permitted: true,
            }], "available", serde_json::json!([{
                "agent_id":"scribe", "workspace":room().to_string(), "profile":"Configured model",
                "eligible":true, "configured":true, "registered":true,
            }])),
        ] {
            let f = discovery_fixture(configured.clone(), registrations);
            let before = f.native.owner.workspace_task_snapshot(room()).unwrap();
            let update = invoke_hook(&f, "desktop:tasks", "read-task-compute", None).await;
            let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
                panic!("published discovery must succeed: {update:?}")
            };
            assert_eq!(output.value.unwrap(), serde_json::json!({
                "setupRequired": status != "available", "status": status,
                "configuredModel": configured, "settings":"settings://agent_models", "workers": workers,
            }));
            assert_eq!(f.native.owner.workspace_task_snapshot(room()).unwrap(), before);
            drop(f);
        }
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn rolebound_nonperson_cannot_invoke_published_compute_read() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = discovery_fixture(None, Vec::new());
            f.state.refresh_workspace_membership_services().unwrap();
            // This is a real role-bound channel, not an actor name in JSON.
            let channel = f.app_channel(101, "so.tilo.agent", true);
            let peer = f.state.workspace_membership_peers.lock().unwrap()[&room().to_string()];
            let mut invoke = channel.invoke_published_action_request();
            {
                let mut p = invoke.get();
                p.set_peer_id(peer);
                p.set_resource("desktop:tasks");
                p.set_action("read-task-compute");
                p.set_has_argument(false);
                p.set_confirmation_json("{}");
            }
            // Policy may refuse at admission or return a retained failed invocation;
            // neither path is allowed to yield person-private discovery metadata.
            let accepted = match invoke.send().promise.await {
                Ok(accepted) => accepted,
                Err(error) => {
                    let message = error.to_string();
                    assert!(
                        message.contains("authenticated person")
                            || message.contains("permission")
                            || message.contains("denied")
                            || message.contains("native task agent requires"),
                        "must be an authority refusal, not unavailable discovery: {message}"
                    );
                    return;
                }
            };
            {
                let accepted = accepted.get().unwrap();
                let first: tilo_actions::InvocationUpdate =
                    serde_json::from_str(accepted.get_update_json().unwrap().to_str().unwrap())
                        .unwrap();
                let invocation = first.invocation();
                let host = f.state.invocation_host().unwrap();
                tokio::time::timeout(Duration::from_secs(2), async {
                    loop {
                        let mut status = channel.published_invocation_observation_request();
                        status.get().set_host(host.as_bytes());
                        status.get().set_invocation(invocation.0.as_bytes());
                        let reply = status.send().promise.await.unwrap();
                        let reply = reply.get().unwrap().get_observation().unwrap();
                        assert!(reply.get_found());
                        let update: tilo_actions::InvocationUpdate = serde_json::from_str(
                            reply.get_update_json().unwrap().to_str().unwrap(),
                        )
                        .unwrap();
                        if update.is_terminal() {
                            let tilo_actions::InvocationUpdate::Failed { error, .. } = update
                            else {
                                panic!("role-bound read must fail: {update:?}")
                            };
                            assert!(
                                error.message.contains("authenticated person")
                                    || error.message.contains("permission")
                                    || error.message.contains("denied"),
                                "{error:?}"
                            );
                            break;
                        }
                        tokio::task::yield_now().await;
                    }
                })
                .await
                .expect("role-bound read must be refused promptly");
            }
            drop(f);
        })
        .await;
}

struct PreparedStart {
    owner: tilo_joined_room::NativeWorkspaceOwner,
    prepared: tilo_joined_room::PreparedNativeTaskStart,
    models: Arc<model_owner::Models>,
    key: SigningKey,
}
impl crate::services::PendingNativeTaskStart for PreparedStart {
    fn approve(&self, executor: crate::services::NativeTaskExecutor) -> CommitFuture {
        let result = self.owner.approve_prepared_task_start(
            &self.prepared,
            &executor.certificate,
            &self.key,
            self.models.as_ref(),
            std::time::SystemTime::now(),
        );
        Box::pin(async move { result })
    }
}
pub(super) fn prepare_start(
    native: &Native,
    request: OwnerCommitRequest,
) -> Result<Box<dyn crate::services::PendingNativeTaskStart>, String> {
    let models = native
        .start_models
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Weak::upgrade)
        .ok_or("MODEL Start authority unavailable")?;
    let prepared =
        native
            .owner
            .prepare_task_start(request, models.as_ref(), std::time::SystemTime::now())?;
    Ok(Box::new(PreparedStart {
        owner: native.owner.clone(),
        prepared,
        models,
        key: native.key.clone(),
    }))
}

#[derive(Default)]
struct Launcher {
    launched: tokio::sync::Notify,
    fail: bool,
}
impl Launcher {
    fn assert_executor(agent: &str, workspace: &tilo_desktop_model::Workspace) {
        assert_eq!(agent, "scribe");
        assert_eq!(workspace.room, room());
    }
}
impl crate::services::LauncherServices for Launcher {
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
        Err("ordinary launch cannot enroll a task executor".into())
    }
    fn launch_task_executor(
        &self,
        agent: &str,
        workspace: &tilo_desktop_model::Workspace,
    ) -> Result<(), String> {
        Self::assert_executor(agent, workspace);
        self.launched.notify_one();
        if self.fail {
            return Err("executor process failed to launch".into());
        }
        Ok(())
    }
    fn attest_process(&self, _: u32, _: &str, _: &str) -> bool {
        false
    }
}

#[path = "native_task_compute_tests/relaunch.rs"]
mod relaunch;

#[derive(Clone, Copy, Debug)]
enum PendingChange {
    None,
    Stop,
    Source,
}

async fn subscribe_during_start(
    f: &Fixture,
    launcher: &Launcher,
    task: &str,
    change: PendingChange,
) -> (
    channel_handle::Client,
    tokio::net::UnixStream,
    Option<task_run_stream::Client>,
) {
    launcher.launched.notified().await;
    let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
    assert!(snapshot.controls[task].active_run.is_none());
    assert!(
        snapshot
            .tasks
            .iter()
            .find(|row| row.id == task)
            .unwrap()
            .assigned_agent
            .is_none(),
        "provisioning must not assign or approve"
    );
    let (managed, socket) = managed_channel(f, 79);
    match change {
        PendingChange::None => {}
        PendingChange::Stop => {
            f.person_command(WorkspaceTaskCommand::CancelForRun {
                task_id: task.to_owned(),
                run_epoch: 0,
                after_revision: 0,
            })
            .unwrap();
        }
        PendingChange::Source => {
            let mut config = agent_provider_config::ProviderConfig::load();
            config.profiles[0].endpoint = "http://127.0.0.1:8123/replaced/v1".into();
            config.save().unwrap();
        }
    }
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
    (managed, socket, Some(stream))
}

#[tokio::test(flavor = "current_thread")]
async fn conscious_published_start_waits_for_subscription_and_revalidates_frozen_owner_custody() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for change in [
                PendingChange::None,
                PendingChange::Stop,
                PendingChange::Source,
            ] {
                let launcher = Arc::new(Launcher::default());
                let registry = Arc::new(crate::services::NativeTaskExecutorRegistry::default());
                let f =
                    Fixture::with_executor_registry(Some(launcher.clone()), Some(registry.clone()));
                assert!(
                    registry.candidates(&room().to_string()).unwrap().is_empty(),
                    "first-use Start must begin with the original empty executor registry"
                );
                let _env = model_owner::configure(&f, "http://127.0.0.1:8123/start/v1".into());
                f.person_command(WorkspaceTaskCommand::QueueWithMetadata {
                    title: "Review the complete document".into(),
                    assigned_agent: None,
                    route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                    metadata: tilo_desktop_model::WorkspaceTaskMetadata::default(),
                })
                .unwrap();
                let task = f
                    .native
                    .owner
                    .workspace_task_snapshot(room())
                    .unwrap()
                    .tasks
                    .iter()
                    .find(|task| task.assigned_agent.is_none())
                    .unwrap()
                    .id
                    .clone();
                let resource = format!("desktop:{task}");
                let start = invoke_hook(
                    &f,
                    &resource,
                    "approve-run",
                    Some(serde_json::json!({
                        "agent_id":"scribe", "expected_run_epoch":0, "after_revision":0
                    })),
                );
                let subscribe = subscribe_during_start(&f, &launcher, &task, change);
                let (result, _connection) =
                    tokio::time::timeout(std::time::Duration::from_secs(10), async {
                        tokio::join!(start, subscribe)
                    })
                    .await
                    .expect("published Start must reach the protected launcher and finish");
                let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
                if matches!(change, PendingChange::None) {
                    assert!(
                        matches!(result, tilo_actions::InvocationUpdate::Succeeded { .. }),
                        "{result:?}"
                    );
                    let run = snapshot.controls[&task].active_run.as_ref().unwrap();
                    assert_eq!(
                        run.approval.as_ref().unwrap().device,
                        f.native.certificate.device_public_key,
                        "the original conscious person's device must approve, never the actor"
                    );
                    assert_eq!(run.agent_id, "scribe");
                    let executor = registry
                        .executor_for_peer(79, &room().to_string(), "scribe")
                        .unwrap();
                    assert_ne!(
                        executor.certificate, f.native.certificate,
                        "the protected subscription must authenticate its own signed actor"
                    );
                    assert_eq!(
                        snapshot
                            .tasks
                            .iter()
                            .find(|row| row.id == task)
                            .unwrap()
                            .assigned_agent
                            .as_deref(),
                        Some("scribe")
                    );
                } else {
                    assert!(
                        matches!(result, tilo_actions::InvocationUpdate::Failed { .. }),
                        "{change:?}: {result:?}"
                    );
                    assert!(snapshot.controls[&task].active_run.is_none());
                    assert!(
                        snapshot
                            .tasks
                            .iter()
                            .find(|row| row.id == task)
                            .unwrap()
                            .assigned_agent
                            .is_none()
                    );
                }
                drop(f);
            }
        })
        .await;
}
