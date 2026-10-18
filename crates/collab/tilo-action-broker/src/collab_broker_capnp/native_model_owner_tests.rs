//! Real protected broker RPC and HTTP transport against an enrolled durable owner.
//! Only host selection/lease plumbing is a fixture; admission uses the production
//! typed MODEL reserve, claim and final dispatch, never a simulated permission bit.
use super::*;
use crate::native_model::{NativeModelHost, NativeModelLease};
use crate::services::{
    FrozenNativeModelSelection, HostDispatchModel, NativeModelAuthority, NativeModelContext,
    NativeModelContract, NativeModelGovernorCustody, TypedNativeModelRequirement,
};
use tilo_joined_room::task_admission::NativeInvocationPermit;
use tilo_model_gateway::{FrozenNativeAttempt, NativeModelDispatch, Request};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[path = "native_model_owner_tests/combined.rs"]
mod combined;
#[path = "native_model_owner_tests/legacy.rs"]
pub(super) mod legacy;
#[path = "native_model_owner_tests/queued.rs"]
mod queued;
#[path = "native_model_owner_tests/receipt.rs"]
mod receipt;
#[path = "native_model_owner_tests/restart.rs"]
mod restart;

type QueuedModel = Arc<Mutex<Option<(TypedNativeModelRequirement, Arc<dyn NativeModelLease>)>>>;

pub(super) struct Models {
    native: Arc<Native>,
    task_windows: std::sync::atomic::AtomicBool,
    pause: Arc<std::sync::atomic::AtomicBool>,
    prepared: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
    queued: QueuedModel,
    closed: Arc<std::sync::atomic::AtomicUsize>,
    close_notify: Arc<tokio::sync::Notify>,
}
impl Models {
    pub(super) fn new(native: Arc<Native>) -> Self {
        Self {
            native,
            task_windows: std::sync::atomic::AtomicBool::new(false),
            pause: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            prepared: Arc::new(tokio::sync::Notify::new()),
            resume: Arc::new(tokio::sync::Notify::new()),
            queued: Arc::new(Mutex::new(None)),
            closed: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            close_notify: Arc::new(tokio::sync::Notify::new()),
        }
    }
}
fn window() -> tilo_action_policy::TreePath {
    tilo_action_policy::TreePath::room(room().to_string())
        .app("so.tilo.chat")
        .kind("windows")
        .resource("chat-window")
}
fn selected_contract(
    profile: &agent_provider_config::ProviderProfile,
    model: &str,
) -> NativeModelContract {
    NativeModelContract {
        window: window(),
        request_model: window().action("requestModel"),
        kind: tilo_action_policy::ActionKind::Outside,
        selection: FrozenNativeModelSelection {
            app: "so.tilo.agent".into(),
            provider: profile.id.clone(),
            model: model.into(),
            contract: serde_json::json!({"profile": profile}),
        },
    }
}
impl tilo_joined_room::task_admission::model::NativeModelStartAuthority for Models {
    fn approved_model_start(
        &self,
        _: &tilo_joined_room::NativeCommittedView,
        task: &str,
        _: &tilo_user_identity::DeviceCert,
        _: std::time::SystemTime,
    ) -> Result<NativeModelContract, String> {
        let config = agent_provider_config::ProviderConfig::load();
        let profile = config.active_profile().ok_or("host selection absent")?;
        let model = profile
            .default_model
            .as_deref()
            .ok_or("host model absent")?;
        Ok(if self.task_windows.load(Ordering::SeqCst) {
            combined::task_contract(profile, model, task)
        } else {
            selected_contract(profile, model)
        })
    }
}
fn model_start_request(f: &Fixture) -> OwnerCommitRequest {
    OwnerCommitRequest {
        room: room().to_string(),
        operation: CommitOperation::WorkspaceTaskCommand {
            invocation: uuid::Uuid::new_v4(),
            authority: WorkspaceTaskAuthority::Person {
                caller: f.native.certificate.clone(),
            },
            command: WorkspaceTaskCommand::ApproveRun {
                task_id: f.task.clone(),
                agent_id: "scribe".into(),
                expected_run_epoch: 0,
                after_revision: 0,
            },
        },
    }
}
fn approve_model(f: &Fixture) {
    f.native
        .owner
        .commit_workspace_task_command_with_model_start(
            model_start_request(f),
            &f.native.key,
            f.models.as_ref(),
            std::time::SystemTime::now(),
        )
        .unwrap();
}
impl NativeModelHost for Models {
    fn claim(
        &self,
        lease: Arc<dyn NativeModelLease>,
        invocation: uuid::Uuid,
    ) -> Result<Arc<dyn NativeModelDispatch>, String> {
        let closed = self.closed.clone();
        let notify = self.close_notify.clone();
        lease.on_close(Box::new(move || {
            closed.fetch_add(1, Ordering::SeqCst);
            notify.notify_one();
        }))?;
        let config = agent_provider_config::ProviderConfig::load();
        let profile = config
            .active_profile()
            .ok_or("host selection absent")?
            .clone();
        let model = profile.default_model.clone().ok_or("host model absent")?;
        Ok(Arc::new(Dispatch {
            profile,
            model,
            native: self.native.clone(),
            task_window: self.task_windows.load(Ordering::SeqCst),
            lease,
            invocation,
            prepared: Mutex::new(None),
            dispatched: Mutex::new(None),
            pause: self.pause.clone(),
            ready: self.prepared.clone(),
            resume: self.resume.clone(),
            queued: self.queued.clone(),
        }))
    }
}
struct Dispatch {
    profile: agent_provider_config::ProviderProfile,
    model: String,
    task_window: bool,
    native: Arc<Native>,
    lease: Arc<dyn NativeModelLease>,
    invocation: uuid::Uuid,
    prepared: Mutex<Option<TypedNativeModelRequirement>>,
    dispatched: Mutex<Option<(TypedNativeModelRequirement, NativeInvocationPermit)>>,
    pause: Arc<std::sync::atomic::AtomicBool>,
    ready: Arc<tokio::sync::Notify>,
    resume: Arc<tokio::sync::Notify>,
    queued: QueuedModel,
}
struct Authority<'a> {
    lease: &'a dyn NativeModelLease,
    contract: NativeModelContract,
}
impl NativeModelAuthority for Authority<'_> {
    fn current_model_context(
        &self,
        view: &tilo_joined_room::NativeCommittedView,
        now: std::time::SystemTime,
    ) -> Result<NativeModelContext, String> {
        self.lease.current(view, self.contract.clone(), now)
    }
}
impl Dispatch {
    fn prepare_exact(&self, attempt: &FrozenNativeAttempt) -> Result<(), String> {
        crate::native_model::attempt_matches(attempt, &self.profile, &self.model)?;
        let contract = if self.task_window {
            combined::task_contract(
                &attempt.profile,
                &attempt.model,
                &self.lease.binding().task_id,
            )
        } else {
            selected_contract(&attempt.profile, &attempt.model)
        };
        let authority = Authority {
            lease: self.lease.as_ref(),
            contract: contract.clone(),
        };
        let context = self.native.owner.with_current(room(), |view| {
            authority.current_model_context(view, std::time::SystemTime::now())
        })?;
        let requirement = TypedNativeModelRequirement::freeze(
            TypedNativeModelRequirement {
                room: room(),
                run: self.lease.binding().clone(),
                invocation: self.invocation,
                request: uuid::Uuid::new_v4(),
                host: self.native.certificate.device_public_key,
                private_run_digest: context.private_run_digest,
                modelselection: contract,
                requestdigest: [0; 32],
            },
            &attempt.body,
        )?;
        *self.queued.lock().unwrap() = Some((requirement.clone(), self.lease.clone()));
        *self.prepared.lock().unwrap() = Some(requirement);
        Ok(())
    }
}
impl NativeModelDispatch for Dispatch {
    fn completed(
        &self,
        proof: &tilo_model_gateway::native_delivery::NativeModelSuccess,
    ) -> Result<(), String> {
        let (requirement, permit) = self
            .dispatched
            .lock()
            .unwrap()
            .take()
            .ok_or("MODEL dispatch absent")?;
        let receipt =
            tilo_joined_room::task_admission::model_delivery::NativeModelDeliveryReceipt::capture(
                proof,
                &requirement,
                &permit,
                &self.native.key,
            )?;
        self.native.owner.record_native_model_delivery(&receipt)
    }
    fn prepare<'a>(
        &'a self,
        invocation: &'a serde_json::Value,
        attempt: &'a FrozenNativeAttempt,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>> {
        Box::pin(async move {
            if serde_json::to_value(self.invocation).unwrap() != *invocation {
                return Err("MODEL invocation changed".into());
            }
            self.prepare_exact(attempt)?;
            // Publish readiness only after registering the resume waiter. The
            // HTTP worker and controller run on different runtime threads.
            let resumed = self.resume.notified();
            tokio::pin!(resumed);
            resumed.as_mut().enable();
            self.ready.notify_one();
            if self.pause.load(Ordering::SeqCst) {
                resumed.await;
            }
            Ok(())
        })
    }
    fn dispatch(
        &self,
        _: &serde_json::Value,
        attempt: &FrozenNativeAttempt,
        start: &mut dyn FnMut() -> Result<(), String>,
    ) -> Result<(), String> {
        let requirement = self
            .prepared
            .lock()
            .unwrap()
            .take()
            .ok_or("MODEL already consumed")?;
        let authority = Authority {
            lease: self.lease.as_ref(),
            contract: requirement.modelselection.clone(),
        };
        let ((), permit) = HostDispatchModel {
            owner: &self.native.owner,
            authority: &authority,
        }
        .dispatch_local(
            &requirement,
            NativeModelGovernorCustody {
                host: &self.native.certificate,
                key: &self.native.key,
                now: std::time::SystemTime::now(),
            },
            &attempt.body,
            |_| start(),
        )?;
        *self.dispatched.lock().unwrap() = Some((requirement, permit));
        Ok(())
    }
}
/// The launcher writes the real handoff before the client gets its descriptor;
/// the production broker authenticates that kernel PID against this live record.
struct RecordedLaunch {
    identity: Arc<dyn crate::services::ManagedApplicationIdentity>,
    protected: launcher::registration_gate::ProtectedModelLaunch,
}
impl crate::services::LauncherServices for RecordedLaunch {
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
        Err("this fixture already holds its launch record".into())
    }
    fn attest_process(&self, pid: u32, app: &str, requested_room: &str) -> bool {
        pid == std::process::id() && app == "so.tilo.agent" && requested_room == room().to_string()
    }
    fn protected_model_launch(
        &self,
        pid: u32,
        app: &str,
        requested_room: &str,
    ) -> Result<Option<launcher::registration_gate::ProtectedModelLaunch>, String> {
        if !self.attest_process(pid, app, requested_room) {
            return Ok(None);
        }
        self.protected
            .validate(app, Some(requested_room))
            .map_err(|error| error.to_string())?;
        Ok(Some(self.protected.clone()))
    }
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        requested_room: &str,
    ) -> Result<Option<crate::services::AuthenticatedApplicationLaunch>, String> {
        Ok(self.attest_process(pid, app, requested_room).then_some(
            crate::services::AuthenticatedApplicationLaunch {
                managed_task_agent: Some("scribe".into()),
                managed_identity: Some(self.identity.clone()),
                creation: None,
                continuation_instance: None,
                launch_ticket: None,
            },
        ))
    }
}
pub(super) fn recorded_launch(native: &Arc<Native>) -> Arc<dyn crate::services::LauncherServices> {
    Arc::new(RecordedLaunch {
        identity: super::compute::enrolled_native_actor(native),
        protected: launcher::registration_gate::ProtectedModelLaunch {
            app: "so.tilo.agent".into(),
            room: room().to_string(),
            agent: "scribe".into(),
            launch: uuid::Uuid::new_v4().to_string(),
        },
    })
}

async fn launched_channel(f: &Fixture) -> (legacy::Connection, runtime_collab::ChannelHandle) {
    let launch = recorded_launch(&f.native);
    let identity = launch
        .authenticated_launch(std::process::id(), "so.tilo.agent", &room().to_string())
        .unwrap()
        .unwrap()
        .managed_identity
        .unwrap();
    let (certificate, delegation, proof) = identity.identity().unwrap();
    assert_eq!(delegation.delegate.agent_id(), Some("scribe"));
    assert!(!certificate.capabilities.can_act_as_host);
    let presentation = runtime_collab::CertifiedJoinIdentity {
        workspace_id: room().to_string(),
        device_certificate: serde_json::to_vec(&certificate).unwrap(),
        proof: proof.to_vec(),
    }
    .encode()
    .unwrap();
    let config = crate::peer_actions::NativePeerBrokerConfig {
        journal_root: f.directory.path().join("launched-journal"),
        certificate_authority: f.native.clone(),
    };
    let (state, rx) = CapnpBrokerState::new_configured(
        &f._fanout,
        Arc::new(Mutex::new(BrokerChannels::default())),
        PeerPublisher::with_optional_owner_commit(None, f.native.clone()),
        Some(f.native.clone()),
        None,
        Some(&config),
        HostServices {
            native_task_executors: Some(Arc::new(
                crate::services::NativeTaskExecutorRegistry::default(),
            )),
            launcher: Some(launch),
            native_model: Some(f.models.clone()),
            task_execution: Some(f.native.clone()),
            workspace_memberships: Some(f.native.clone()),
            ..Default::default()
        },
    );
    let directories = Rc::new(LocalActionDirectoryRegistry::new(rx));
    let route = CrdtRoute::record("so.tilo.agent", room().to_string(), "main");
    let (mut stream, _authorize) = state
        .connect_local_worker_with_authorizer(route, &certificate)
        .unwrap();
    let pending = state.workers.lock().unwrap().pop().unwrap();
    launcher::services::write_collab_handoff(&mut stream, "so.tilo.agent", &room().to_string())
        .unwrap();
    let mut server = tokio::net::UnixStream::from_std(pending.stream).unwrap();
    let driver = tokio::task::spawn_local(async move {
        let handoff = authenticate_launcher_handoff(&mut server, &state.services)
            .await
            .map_err(|r| r.error)?;
        let _exit = ConnectionExit {
            state: state.clone(),
            directories: directories.clone(),
            peer_id: 7200,
        };
        let _monitor =
            native_media::ProcessMonitor::start(handoff.local_media.clone(), &state, 7200);
        let service: collab_service::Client = capnp_rpc::new_client(CapnpCollabService {
            worker: Some(pending.binding),
            lease: Rc::new(ConnectionLease {
                local_media: handoff.local_media,
                state: state.clone(),
                directories: directories.clone(),
                peer_id: 7200,
            }),
            state,
            directories,
            peer_id: 7200,
        });
        runtime_portal::serve_connected_io(server, service.client).await
    });
    let broker = legacy::Connection(driver);
    stream.set_nonblocking(true).unwrap();
    let stream = tokio::net::UnixStream::from_std(stream).unwrap();
    let client = runtime_collab::CollabClient::from_raw(
        runtime_portal::CapnpClient::from_connected_io(
            stream,
            capnp_rpc::rpc_twoparty_capnp::Side::Client,
        )
        .bootstrap(),
    );
    let route = CrdtRoute::record("so.tilo.agent", room().to_string(), "main");
    let channel = client
        .join_channel_as_kind(&route.channel_key(), &presentation, "scribe", "agent", "")
        .await
        .unwrap();
    (broker, channel)
}
#[tokio::test(flavor = "current_thread")]
async fn approved_profile_is_private_and_survives_owner_restart() {
    tokio::task::LocalSet::new().run_until(async {
        let f = Fixture::new();
        let _env = configure(&f, "http://127.0.0.1:8123/private-profile/v1".into());
        let request = model_start_request(&f);
        let now = std::time::SystemTime::now();
        let signed = tilo_joined_room::task_admission::model_start::NativeModelPersonStartSnapshot::capture(
            &f.native.owner, &request, f.models.as_ref(), &f.native.key, now,
        ).unwrap();
        f.native.owner.with_current(room(), |view| {
            signed.verify_for_start(view.peer(), &f.native.certificate, &request, now)?;
            let mut replaced = request.clone();
            let CommitOperation::WorkspaceTaskCommand { command, .. } = &mut replaced.operation else { unreachable!() };
            let WorkspaceTaskCommand::ApproveRun { expected_run_epoch, .. } = command else { unreachable!() };
            *expected_run_epoch += 1;
            assert!(signed.verify_for_start(view.peer(), &f.native.certificate, &replaced, now).is_err());
            assert!(signed.verify_for_start(view.peer(), &f.native.certificate, &request, now + Duration::from_secs(121)).is_err());
            let mut forged = serde_json::to_value(&signed).unwrap();
            forged["contract"]["selection"]["model"] = serde_json::json!("replacement");
            let forged: tilo_joined_room::task_admission::model_start::NativeModelPersonStartSnapshot = serde_json::from_value(forged).unwrap();
            assert!(forged.verify_for_start(view.peer(), &f.native.certificate, &request, now).is_err());
            Ok(())
        }).unwrap();
        signed.commit(&f.native.owner, request, NativeModelGovernorCustody {
            host: &f.native.certificate, key: &f.native.key, now,
        }).unwrap();
        let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
        let run = snapshot.controls[&f.task].active_run.as_ref().unwrap();
        let binding = NativeTaskBinding {
            task_id: f.task.clone(), agent_id: run.agent_id.clone(), epoch: run.epoch,
            revision: run.applied_direction_revision,
            starter: run.approval.as_ref().map(|approval| approval.person.clone()),
        };
        let public = serde_json::to_string(&snapshot).unwrap();
        assert!(!public.contains("owner-loopback"));
        assert!(!public.contains("private-profile"));
        assert!(tilo_peer_state::workspace_task::is_private_model_contract_reference(
            &snapshot.controls[&f.task].model_authority.as_ref().unwrap().contract,
        ));
        f.native.owner.with_current(room(), |view| {
            let shared = tilo_peer_state::workspace_task::snapshot(view.peer().admitted_room().unwrap())?;
            assert_eq!(shared.controls[&f.task].model_authority, snapshot.controls[&f.task].model_authority);
            Ok(())
        }).unwrap();
        let authority = WorkspaceTaskAuthority::Person { caller: f.native.certificate.clone() };
        let private = f.native.owner.workspace_task_private_model_contract(
            room(), &binding, &authority, std::time::SystemTime::now(),
        ).unwrap();
        assert!(serde_json::to_string(&private).unwrap().contains("private-profile"));
        let reopened = tilo_joined_room::open_file(&f.directory.path().join("native.json")).unwrap();
        assert_eq!(reopened.workspace_task_private_model_contract(
            room(), &binding, &authority, std::time::SystemTime::now(),
        ).unwrap(), private);
        let mut wrong = binding;
        wrong.epoch += 1;
        assert!(reopened.workspace_task_private_model_contract(
            room(), &wrong, &authority, std::time::SystemTime::now(),
        ).is_err());
    }).await;
}

async fn delivered_live(
    channel: &runtime_collab::ChannelHandle,
) -> runtime_collab::TaskRunEnvelope {
    let runs = channel.subscribe_task_runs().await.unwrap();
    tokio::time::timeout(Duration::from_secs(3), runs.read())
        .await
        .unwrap()
        .unwrap()
}
pub(super) fn configure(f: &Fixture, endpoint: String) -> tilo_test_env::ScopedEnv {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("XDG_CONFIG_HOME", f.directory.path());
    let mut profile = agent_provider_config::ProviderProfile::local_vllm();
    profile.id = "owner-loopback".into();
    profile.endpoint = endpoint;
    profile.models = vec![agent_provider_config::ModelDescriptor::new("selected")];
    profile.default_model = Some("selected".into());
    agent_provider_config::ProviderConfig {
        default_profile: Some(profile.id.clone()),
        profiles: vec![profile],
    }
    .save()
    .unwrap();
    put_grant(
        f,
        tilo_action_policy::Grant::new(
            "fixture-model-window",
            tilo_action_policy::Subject::agent("scribe"),
            window().action("requestModel"),
            tilo_action_policy::KindSet::ALL,
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Subject::person(hex::encode(f.native.certificate.user_public_key)),
        ),
    );
    env
}
fn request(id: serde_json::Value, transform: bool) -> Request {
    if transform {
        Request::NativeTransform {
            invocation: id,
            profile_id: "owner-loopback".into(),
            model_id: "selected".into(),
            instructions: "Return only the revised text".into(),
            input: "private draft".into(),
            max_output_tokens: 16,
            timeout_ms: 2000,
        }
    } else {
        Request::NativePlan {
            invocation: id,
            profile_id: "owner-loopback".into(),
            model_id: "selected".into(),
            body: serde_json::json!({"model":"selected","messages":[{"role":"user","content":"private draft"}],"max_tokens":16}),
            timeout_ms: 2000,
            stream: false,
            deltas: false,
            trace: None,
        }
    }
}
async fn read_post(socket: &mut tokio::net::TcpStream) -> serde_json::Value {
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") {
        head.push(socket.read_u8().await.unwrap());
        assert!(head.len() < 65536);
    }
    let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
    assert!(head.starts_with("post "));
    let length: usize = head
        .lines()
        .find_map(|line| line.strip_prefix("content-length:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(length < 1024 * 1024);
    let mut body = vec![0; length];
    socket.read_exact(&mut body).await.unwrap();
    serde_json::from_slice(&body).unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn protected_broker_plan_and_transform_reach_real_owner_and_loopback() {
    tokio::task::LocalSet::new().run_until(async {
        let mut f = Fixture::new();
        let source = uuid::Uuid::new_v4();
        f.person_command(WorkspaceTaskCommand::Queue {
            title: "Review attached document".into(),
            assigned_agent: "scribe".into(),
            route: tilo_desktop_model::WorkspaceTaskRoute::Model,
            resource: Some(Box::new(tilo_desktop_model::WorkspaceTaskResource {
                app_id: Some("so.tilo.notes".into()), app_label: Some("Notes".into()),
                kind: "document".into(), id: "attached-document".into(),
                label: "Proposal".into(),
                agent_request: Some(tilo_desktop_model::WorkspaceAgentRequest {
                    chat_record: "planning-conversation".into(), channel_id: "planning".into(),
                    message_id: format!("m_{source}"),
                    instruction: "Review only this attached document".into(),
                    invocation: tilo_actions::InvocationId(source),
                }),
            })),
        }).unwrap();
        f.task = f.native.owner.workspace_task_snapshot(room()).unwrap().tasks.into_iter()
            .find(|task| task.resource.as_ref().is_some_and(|resource| resource.id == "attached-document"))
            .unwrap().id;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
        approve_model(&f);
        let snapshot = f.native.owner.workspace_task_snapshot(room()).unwrap();
        let control = &snapshot.controls[&f.task];
        let data = &control.execution_authority.as_ref().unwrap().scope;
        assert_eq!(*data, tilo_action_policy::TreePath::room(room().to_string())
            .app("so.tilo.notes").kind("document").resource("attached-document"));
        assert!(!data.covers(&window().action("requestModel")));
        assert!(!data.covers(&tilo_action_policy::TreePath::room(room().to_string())
            .app("so.tilo.notes").kind("document").resource("unrelated-document")));
        let (broker, channel) = launched_channel(&f).await;
        let run = delivered_live(&channel).await;
        let peer = tokio::spawn(async move {
            for _ in 0..2 {
                let (mut socket, _) = listener.accept().await.unwrap();
                let body = read_post(&mut socket).await;
                assert_eq!(body["model"], "selected");
                assert!(body.to_string().contains("private draft"));
                let reply = r#"{"id":"answer","object":"chat.completion","created":1,"model":"selected","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1}}"#;
                socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).as_bytes()).await.unwrap();
            }
            listener
        });
        for transform in [false, true] {
            let id = channel.claim_native_model(run.run_id, &run.lease).await.unwrap();
            let request = serde_json::to_value(request(id, transform)).unwrap();
            let reply = channel.execute_native_model(&request).await.unwrap();
            assert_eq!(reply["response"]["ok"], true, "{reply}");
            assert!(channel.execute_native_model(&request).await.is_err(), "an invocation cannot POST twice");
            assert_eq!(f.models.closed.load(Ordering::SeqCst), 0,
                "finishing an inference must not revoke the reused delivered connection");
        }
        let listener = tokio::time::timeout(Duration::from_secs(3), peer).await.unwrap().unwrap();
        queued::assert_no_http(&listener).await;
        drop(channel);
        // Close the actual transport, not just one imported channel capability.
        // The RPC driver otherwise still owns the connected socket.
        drop(broker);
        tokio::time::timeout(Duration::from_secs(3), async {
            while f.models.closed.load(Ordering::SeqCst) != 2 {
                f.models.close_notify.notified().await;
            }
        }).await.expect("disconnect closes both retained MODEL claims");
    }).await;
}

#[tokio::test(flavor = "current_thread")]
async fn stop_after_real_model_claim_before_transport_submission_posts_zero_bytes() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let f = Fixture::new();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let _env = configure(&f, format!("http://{}/v1", listener.local_addr().unwrap()));
            approve_model(&f);
            f.models.pause.store(true, Ordering::SeqCst);
            let (_broker, channel) = launched_channel(&f).await;
            let run = delivered_live(&channel).await;
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
            // This closes the actual durable governor ledger, not the adapter's
            // lease or an HTTP counter. A previously claimed permit cannot pass it.
            let epoch = f
                .native
                .owner
                .workspace_task_snapshot(room())
                .unwrap()
                .controls[&f.task]
                .run_epoch;
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
            let reply = tokio::time::timeout(Duration::from_secs(3), executing)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(reply["response"]["ok"], false, "{reply}");
            // TCP may already have connected before custody preparation. Inspect
            // the actual peer socket: neither headers nor body may have been sent.
            let (mut socket, _) = tokio::time::timeout(Duration::from_secs(3), listener.accept())
                .await
                .unwrap()
                .unwrap();
            let mut bytes = Vec::new();
            tokio::time::timeout(Duration::from_secs(3), socket.read_to_end(&mut bytes))
                .await
                .unwrap()
                .unwrap();
            assert!(bytes.is_empty(), "a stopped queued attempt sent HTTP bytes");
        })
        .await;
}
