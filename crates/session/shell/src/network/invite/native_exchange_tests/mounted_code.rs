//! Fresh resource actions cross real native sessions while both Code roots stay mounted.
use super::*;
use crate::network::actor::{Actor, NetCmd};
use crate::shell::network::ShellNetwork;
use cold_code::{APP, BODY, FILE, LocalSurface, MountedCode};
use sha2::{Digest as _, Sha256};

#[path = "mounted_code/acknowledgement_recovery.rs"]
mod acknowledgement_recovery;
#[path = "mounted_code/active_reads.rs"]
mod active_reads;
#[path = "mounted_code/blocked_read.rs"]
mod blocked_read;
#[path = "mounted_code/idle_reads.rs"]
mod idle_reads;
#[path = "mounted_code/read_recovery.rs"]
mod read_recovery;

// The mounted UI stays on the caller thread while Iroh and persistence can
// progress independently, as on ShellNetwork's two-worker runtime.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_resource_edit_reaches_another_mounted_app() {
    trace_publication_hops();
    Box::pin(super::actual_exchange(false, true, Some(Journey::Document))).await;
}

/// A verb that queues work for the device running the project's terminal
/// (`run`) is a document job: it reaches the mounted app that owns the
/// document, applies there and is answered when what it queued settles.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_run_is_applied_and_settles() {
    trace_publication_hops();
    Box::pin(super::actual_exchange(false, true, Some(Journey::Run))).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_has_acknowledged_private_continuity() {
    trace_publication_hops();
    Box::pin(super::actual_exchange(
        false,
        true,
        Some(Journey::PrivateAdmission),
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_mounted_code_refresh_survives_a_blocked_ui_caller() {
    trace_publication_hops();
    Box::pin(super::actual_exchange(
        false,
        true,
        Some(Journey::BlockedUiRefresh),
    ))
    .await;
}

fn trace_publication_hops() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("warn,tilo_joined_room::session::cursor=debug,tilo_joined_room::testing::governor=debug,tilo_relay::native_publication=debug")
        .try_init();
}

#[derive(Clone, Copy)]
pub(super) enum Journey {
    Document,
    PrivateAdmission,
    BlockedRead,
    BlockedUiRefresh,
    ReadRecovery,
    Run,
    AcknowledgementRecovery,
    IdleReads,
    IdleReadsAnnounced,
    /// A collaborator edits a shared document of this many lines.
    ActiveReads {
        lines: usize,
    },
}

pub(super) struct Context<'a> {
    pub journey: Journey,
    pub directory: &'a tempfile::TempDir,
    pub relay: &'a tilo_relay::Relay,
    pub publisher: &'a Arc<tilo_relay::RelayCoeditSession>,
    pub server: iroh::EndpointAddr,
    pub endpoint: &'a iroh::Endpoint,
}

/// The actual owner services authenticated refreshes independently of UI frames.
/// Completion errors are retained and surfaced by the journey, never discarded.
struct RefreshConsumer {
    outcomes: tokio::sync::mpsc::Receiver<Result<(), String>>,
    received: Arc<std::sync::atomic::AtomicBool>,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl RefreshConsumer {
    fn start(
        mut requests: tokio::sync::mpsc::Receiver<tilo_relay::peer::sync_refresh::Request>,
        authority: ActualAuthority,
        publisher: Arc<tilo_relay::RelayCoeditSession>,
    ) -> Self {
        let (completed, outcomes) = tokio::sync::mpsc::channel(32);
        let received = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = received.clone();
        let task = tokio::spawn(async move {
            while let Some(request) = requests.recv().await {
                observed.store(true, std::sync::atomic::Ordering::Release);
                let result = super::refresh::refresh_shared_on_request(&authority, request).await;
                if result.is_ok() {
                    publish(&authority, &publisher);
                }
                let failed = result.is_err();
                if completed.send(result).await.is_err() || failed {
                    break;
                }
            }
        });
        Self {
            outcomes,
            received,
            task: Some(task),
        }
    }

    async fn block_ui_after_request(&self) {
        tilo_test_env::within(
            "a real authenticated checkpoint refresh to reach its owner",
            async {
                while !self.received.load(std::sync::atomic::Ordering::Acquire) {
                    tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                }
            },
        )
        .await;
        // Only the UI caller is occupied. The original three-second refresh
        // gate and the native network workers retain their production clocks.
        std::thread::sleep(std::time::Duration::from_millis(3200));
    }

    async fn close(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            if let Err(error) = task.await {
                assert!(error.is_cancelled(), "refresh owner task failed: {error}");
            }
        }
        while let Ok(result) = self.outcomes.try_recv() {
            result.expect("the actual refresh owner must not lose a failed completion");
        }
    }
}

impl Drop for RefreshConsumer {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

struct Device {
    owner: NativeWorkspaceOwner,
    identity: HostIdentity,
    endpoint: iroh::Endpoint,
}

struct Mounted {
    code: MountedCode,
    network: Arc<ShellNetwork>,
    actor: tokio::task::JoinHandle<()>,
    acknowledgement_gate: Option<Arc<acknowledgement_recovery::Gate>>,
    idle_reads: Option<Arc<idle_reads::Counter>>,
    active_reads: Option<Arc<active_reads::Meter>>,
}

impl Mounted {
    async fn new_with_acknowledgement_gate(
        device: &Device,
        profile: &std::path::Path,
        acknowledgement_gate: Option<Arc<acknowledgement_recovery::Gate>>,
    ) -> Self {
        Self::new_observed(device, profile, acknowledgement_gate, None, None).await
    }
    async fn new_observed(
        device: &Device,
        profile: &std::path::Path,
        acknowledgement_gate: Option<Arc<acknowledgement_recovery::Gate>>,
        idle_reads: Option<Arc<idle_reads::Counter>>,
        active_reads: Option<Arc<active_reads::Meter>>,
    ) -> Self {
        let (commands, actor) = Actor::spawn_reporting_addrs(
            device.endpoint.clone(),
            iroh::address_lookup::memory::MemoryLookup::new(),
            Some(device.identity.clone()),
            Arc::new(Mutex::new(Vec::new())),
        );
        let (mut network, unused_commands) = crate::shell::network::native_read_network_for_test(
            crate::shell::native_workspace_owner::test_tree_of(&device.owner),
            device.identity.clone(),
        );
        drop(unused_commands);
        network.cmd_tx = commands;
        // Resource actions reach the governor through the network runtime;
        // without one the first invocation is refused and the app's
        // collaboration fails closed for the rest of the journey (#4001).
        crate::shell::network::with_test_runtime(&mut network);
        let network = Arc::new(network);
        let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        network
            .cmd_tx
            .send(NetCmd::JoinWorkspace {
                ws: tilo_relay::room_workspace_id(&room.to_string()).unwrap(),
                on_event: network.event_tx.clone(),
            })
            .await
            .unwrap();
        network
            .cmd_tx
            .send(NetCmd::NativeConnect(
                crate::network::actor::native_ingress::Connect::new(device.owner.clone(), room)
                    .unwrap(),
            ))
            .await
            .unwrap();
        let code = cold_code::mount_with_network_and_reads(
            &device.owner,
            &device.identity,
            profile,
            &network,
            acknowledgement_gate
                .as_ref()
                .map(acknowledgement_recovery::Gate::wrapper)
                .or_else(|| idle_reads.as_ref().map(idle_reads::Counter::wrapper))
                .or_else(|| active_reads.as_ref().map(active_reads::Meter::wrapper)),
        );
        Self {
            code,
            network,
            actor,
            acknowledgement_gate,
            idle_reads,
            active_reads,
        }
    }

    fn frame(&mut self) {
        self.network.tick();
        self.code
            .app
            .surface_frame(&LocalSurface, &mut ui::vello::Scene::new());
        self.code.app.refresh_semantic_tree();
    }

    fn invoke(&mut self, action: &str, argument: Option<&str>) {
        self.invoke_where(action, argument, |id| id.contains(FILE));
    }

    /// Invoke `action` on the first published node whose id `on` accepts.
    fn invoke_where(&mut self, action: &str, argument: Option<&str>, on: impl Fn(&str) -> bool) {
        self.frame();
        let node = self
            .code
            .app
            .semantic_tree()
            .iter()
            .find(|node| on(&node.id.0) && node.action(action).is_some())
            .expect("the original mounted Code resource must export this action")
            .id
            .clone();
        assert!(
            self.code
                .app
                .invoke_semantic_action(&node, action, argument)
                .unwrap()
        );
    }

    fn has_body(&self, text: &str) -> bool {
        self.code
            .app
            .controlled_native_input_snapshot_for_test()
            .is_some_and(|snapshot| {
                snapshot["ready"] == true
                    && snapshot["buffer_id"] == FILE
                    && snapshot["body_hash"] == hex::encode(Sha256::digest(text.as_bytes()))
                    && snapshot["body_bytes"] == text.len()
            })
    }

    fn close(self) -> impl std::future::Future<Output = ()> {
        let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
        let Self {
            code,
            network,
            actor,
            acknowledgement_gate: _,
            idle_reads: _,
            active_reads: _,
        } = self;
        drop(code);
        async move {
            network
                .cmd_tx
                .send(NetCmd::LeaveWorkspace {
                    ws: tilo_relay::room_workspace_id(&room.to_string()).unwrap(),
                })
                .await
                .unwrap();
            actor.abort();
            let _ = actor.await;
        }
    }
}

/// What the journey does once both roots show the file: its own read,
/// recovery or run, else an edit both devices converge on.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn finish_journey(
    context: &Context<'_>,
    authority: &ActualAuthority,
    first: &Device,
    owner: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
) {
    if matches!(
        context.journey,
        Journey::BlockedRead | Journey::BlockedUiRefresh
    ) {
        blocked_read::run(first, &mut mounted[0]).await;
    } else if matches!(context.journey, Journey::ReadRecovery) {
        read_recovery::run(first, &mut mounted[0]).await;
    } else if matches!(context.journey, Journey::Run) {
        run_and_settle(&mut mounted[0]).await;
    } else if matches!(
        context.journey,
        Journey::IdleReads | Journey::IdleReadsAnnounced
    ) {
        idle_reads::run(
            first,
            &mut mounted[0],
            matches!(context.journey, Journey::IdleReadsAnnounced),
        )
        .await;
    } else if let Journey::ActiveReads { lines } = context.journey {
        active_reads::run(first, &mut mounted[0], lines).await;
    } else {
        edit_and_converge(context, authority, owner, refreshes, mounted).await;
    }
}

#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
pub(super) async fn run(
    context: Context<'_>,
    owner: NativeWorkspaceOwner,
    identity: &HostIdentity,
    authority: ActualAuthority,
    receiving_custody: Arc<Mutex<Option<NativeWorkspaceOwner>>>,
) {
    eprintln!("mounted journey: enrolling second device");
    let second = Box::pin(enroll_second(&context, &authority, &receiving_custody)).await;
    let receipts = super::receipts::attach(
        context.directory.path(),
        authority.clone(),
        context.publisher.clone(),
    )
    .await;
    let first = Device {
        owner,
        identity: identity.clone(),
        endpoint: context.endpoint.clone(),
    };
    let active_writer = active_reads::enroll_writer(&context, &authority, &receiving_custody).await;
    eprintln!("mounted journey: mounting both original Code roots");
    let mut mounted = vec![
        Mounted::new_observed(
            &first,
            &context.directory.path().join("mounted-first-profile"),
            None,
            matches!(
                context.journey,
                Journey::IdleReads | Journey::IdleReadsAnnounced
            )
            .then(idle_reads::Counter::new),
            matches!(context.journey, Journey::ActiveReads { .. }).then(active_reads::Meter::new),
        )
        .await,
        Mounted::new_with_acknowledgement_gate(
            &second,
            &context.directory.path().join("mounted-second-profile"),
            matches!(context.journey, Journey::AcknowledgementRecovery)
                .then(|| acknowledgement_recovery::Gate::new(&second.owner)),
        )
        .await,
    ];
    let (mailbox, requests) = tilo_relay::peer::sync_refresh::Mailbox::bounded();
    let topic = tilo_relay::room_workspace_id(tilo_joined_room::testing::ROOM).unwrap();
    context
        .relay
        .install_sync_refresh(topic, mailbox)
        .await
        .unwrap();
    let mut refreshes =
        RefreshConsumer::start(requests, authority.clone(), context.publisher.clone());
    publish(&authority, context.publisher);
    // Showing the file is the editor's own effect, which the effect broker
    // signs a receipt for; the framework's `open` is a view and signs none.
    let show =
        tilo_resource::Effect::id(&code_editor::CodeEditorEffect::ShowFile { file: FILE.into() });
    for app in &mut mounted {
        app.invoke(show, None);
    }
    if matches!(context.journey, Journey::BlockedUiRefresh) {
        refreshes.block_ui_after_request().await;
    }
    drive(
        &context,
        &authority,
        &receipts.owner,
        &mut refreshes,
        &mut mounted,
        BODY,
        2,
    )
    .await;
    inspect_while_ui_caller_is_blocked(&context, &authority);
    finish_journey(
        &context,
        &authority,
        active_writer.as_ref().unwrap_or(&first),
        &receipts.owner,
        &mut refreshes,
        &mut mounted,
    )
    .await;
    let private_routes = mounted
        .iter_mut()
        .map(|app| app.code.app.controlled_private_continuity_route_for_test())
        .collect::<Vec<_>>();
    refreshes.close().await;
    for app in mounted {
        app.close().await;
    }
    receipts.close().await;
    second.endpoint.close().await;
    active_reads::close_writer(active_writer).await;
    if matches!(context.journey, Journey::PrivateAdmission) {
        assert!(
            private_routes.iter().all(Option::is_some),
            "both original mounted native Apps require an acknowledged private continuity route; observed {private_routes:?}"
        );
        assert!(
            private_routes
                .iter()
                .flatten()
                .all(|route| route.record.as_deref() == Some(cold_code::RECORD)),
            "private admission stays on the original document route, without a continuation-label shortcut"
        );
    }
}

#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn edit_and_converge(
    context: &Context<'_>,
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
) {
    report_owner_phases("before edit", mounted);
    let edited = format!("{BODY}// edited through a mounted resource action\n");
    mounted[0].invoke("replace-text", Some(&edited));
    drive(context, authority, source, refreshes, mounted, &edited, 2).await;
    report_owner_phases("after first edit", mounted);
    // A second fresh edit proves the receiving root remains connected after
    // its first remote merge; no app remount or record-graph copy occurs.
    let edited_again = format!("{edited}// second mounted device edits\n");
    if let Some(gate) = &mounted[1].acknowledgement_gate {
        gate.arm(&edited_again);
    }
    mounted[1].invoke("replace-text", Some(&edited_again));
    drive(
        context,
        authority,
        source,
        refreshes,
        mounted,
        &edited_again,
        2,
    )
    .await;
    if let Some(gate) = &mounted[1].acknowledgement_gate {
        gate.assert_observed();
    }
}

fn report_owner_phases(stage: &str, mounted: &[Mounted]) {
    eprintln!(
        "owner merge phases {stage}: {:?}",
        mounted
            .iter()
            .map(|app| app.code.app.controlled_document_merge_phase_for_test())
            .collect::<Vec<_>>()
    );
}

async fn enroll_second(
    context: &Context<'_>,
    authority: &ActualAuthority,
    receiving_custody: &Arc<Mutex<Option<NativeWorkspaceOwner>>>,
) -> Device {
    Box::pin(enroll_device(
        context,
        authority,
        receiving_custody,
        83,
        "Second mounted device",
    ))
    .await
}

async fn enroll_device(
    context: &Context<'_>,
    authority: &ActualAuthority,
    receiving_custody: &Arc<Mutex<Option<NativeWorkspaceOwner>>>,
    key_seed: u8,
    label: &str,
) -> Device {
    let key = ed25519_dalek::SigningKey::from_bytes(&[key_seed; 32]);
    let root = ed25519_dalek::SigningKey::from_bytes(&[0xC2; 32]);
    let certificate = tilo_joined_room::testing::certificate(&root, &key, label);
    let invitation = {
        let mut state = authority.lock().unwrap();
        let state = state.as_mut().unwrap();
        state.issue(
            vec![state.hint(context.server.clone())],
            MemberRole::Editor,
            Some(certificate.user_public_key),
            Some(certificate.device_label.clone()),
            now_ns(SystemTime::now())
                .unwrap()
                .checked_add(60_000_000_000)
                .unwrap(),
        )
    };
    let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
    let slot = tree.desktop.active;
    tree.persist_path = Some(
        context
            .directory
            .path()
            .join(format!("mounted-{key_seed}-state.json")),
    );
    let owner = crate::shell::native_workspace_owner::open(tree.shared()).unwrap();
    *receiving_custody.lock().unwrap() = Some(owner.clone());
    let endpoint = iroh::Endpoint::builder(iroh::endpoint::presets::Minimal)
        .secret_key(iroh::SecretKey::from_bytes(&key.to_bytes()))
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr((std::net::Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .bind()
        .await
        .unwrap();
    let identity = HostIdentity {
        device_cert: certificate,
        device_signing_key: key,
        workspace_device_certs: std::collections::HashMap::new(),
        workspace_revocations: std::collections::HashMap::new(),
        identity_paths: None,
    };
    let enrollment = prepare_and_complete(
        WorkspaceJoinClient::new(endpoint.clone()),
        identity.clone(),
        tilo_relay::room_workspace_id(tilo_joined_room::testing::ROOM).unwrap(),
        Some(slot),
        join::encode_invitation_text(&invitation),
        owner.clone(),
    )
    .await
    .unwrap();
    owner.install(&enrollment, SystemTime::now()).unwrap();
    let access = authority
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .governed_access(SystemTime::now())
        .unwrap();
    context
        .relay
        .replace_governed_workspace_access(
            tilo_relay::room_workspace_id(tilo_joined_room::testing::ROOM).unwrap(),
            true,
            access,
            context.directory.path().join("relay"),
        )
        .await
        .unwrap();
    Device {
        owner,
        identity,
        endpoint,
    }
}

fn inspect_while_ui_caller_is_blocked(context: &Context<'_>, authority: &ActualAuthority) {
    let invitation = {
        let mut state = authority.lock().unwrap();
        let state = state.as_mut().unwrap();
        state.issue(
            vec![state.hint(context.server.clone())],
            MemberRole::Viewer,
            None,
            None,
            now_ns(SystemTime::now())
                .unwrap()
                .checked_add(60_000_000_000)
                .unwrap(),
        )
    };
    let client = WorkspaceJoinClient::new(context.endpoint.clone());
    let server = context.server.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    tokio::spawn(async move {
        let result = client.inspect(server, invitation).await;
        let _ = sender.send(result);
    });
    // Rendering and CRDT work occupy the UI caller synchronously. A real
    // invitation inspection must still complete on the network workers.
    let inspection = tilo_test_env::recv("join inspection while mounted UI is blocked", &receiver)
        .expect("the actual workspace join service must answer independently of UI frames");
    assert_eq!(inspection.grant.room, tilo_joined_room::testing::ROOM);
    assert_eq!(inspection.grant.role, MemberRole::Viewer);
}

fn publish(authority: &ActualAuthority, publisher: &tilo_relay::RelayCoeditSession) {
    let state = authority.lock().unwrap();
    super::refresh::publish_committed(state.as_ref().unwrap(), publisher);
}

#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn drive(
    context: &Context<'_>,
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    refreshes: &mut RefreshConsumer,
    mounted: &mut [Mounted],
    body: &str,
    audits: usize,
) {
    let stage_started = std::time::Instant::now();
    eprintln!(
        "mounted journey: awaiting {}",
        hex::encode(Sha256::digest(body.as_bytes()))
    );
    tilo_test_env::within("both continuously mounted native Code roots to converge", async {
        let mut reported = std::time::Instant::now();
        loop {
            if reported.elapsed() >= std::time::Duration::from_secs(20) {
                report_snapshots(authority, source, mounted, stage_started.elapsed());
                reported = std::time::Instant::now();
            }
            for app in &mut *mounted { app.frame(); }
            let signed = signed_audits(source);
            if signed.len() == audits
                && has_committed_body(source, body)
                && mounted.iter().all(|app| app.has_body(body)
                    && app.code.app.collaboration_is_connected()
                    && signed_audits(&app.network.native_owner().unwrap()) == signed
                    && has_committed_body(&app.network.native_owner().unwrap(), body))
            {
                eprintln!("mounted convergence after {:?}: {} bytes; {audits} exact signed terminal audits on both live native owners", stage_started.elapsed(), body.len());
                report_snapshots(authority, source, mounted, stage_started.elapsed());
                break;
            }
            tokio::select! {
                result = refreshes.outcomes.recv() => {
                    result.expect("the actual refresh owner must remain live").unwrap();
                }
                Some(tilo_relay::Inbound::Delta { author, data }) = context.publisher.recv() => {
                    let message: NeighbourMsg = postcard::from_bytes(&data).unwrap();
                    if let NeighbourMsg::AppDocument { app_id, .. } = &message
                        && app_id == APP
                        && author != authority.lock().unwrap().as_ref().unwrap().governor().device_public_key
                    {
                        let admission = source.receive(RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(), tilo_peer_state::TransportOrigin::RelayDelta(author), message.clone(), SystemTime::now()).unwrap();
                        eprintln!("mounted native governor input {}: {:?}", hex::encode(blake3::hash(&data).as_bytes()), admission.primary());
                        {
                            let mut state = authority.lock().unwrap();
                            let state = state.as_mut().unwrap();
                            state.receive(tilo_peer_state::TransportOrigin::RelayDelta(author), message).unwrap();
                            state.refresh_checkpoint().unwrap();
                        }
                        publish(authority, context.publisher);
                    }
                }
                () = tokio::time::sleep(std::time::Duration::from_millis(10)) => {}
            }
        }
    }).await;
}

fn signed_audits(owner: &NativeWorkspaceOwner) -> std::collections::BTreeSet<uuid::Uuid> {
    owner
        .with_current(
            RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            |view| {
                Ok(
                    tilo_desktop_model::projection::workspace_effect_receipts_of(
                        view.peer().admitted_room().unwrap(),
                    )
                    .into_iter()
                    .map(|receipt| receipt.invocation.0)
                    .collect(),
                )
            },
        )
        .unwrap()
}

fn has_committed_body(owner: &NativeWorkspaceOwner, expected: &str) -> bool {
    owner
        .with_current(
            RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            |view| {
                Ok(view
                    .peer()
                    .document(APP, cold_code::RECORD)
                    .is_some_and(|bytes| {
                        cold_code::body(&automerge::AutoCommit::load(&bytes).unwrap()) == expected
                    }))
            },
        )
        .unwrap()
}

fn report_snapshots(
    authority: &ActualAuthority,
    source: &NativeWorkspaceOwner,
    mounted: &[Mounted],
    elapsed: std::time::Duration,
) {
    let publisher = authority.lock().unwrap();
    if let Some(publisher) = publisher.as_ref() {
        let document = publisher.peer().document(APP, cold_code::RECORD).map(|bytes| {
            let mut document = automerge::AutoCommit::load(&bytes).unwrap();
            let body = cold_code::body(&document);
            serde_json::json!({
                "body_hash": hex::encode(Sha256::digest(body.as_bytes())),
                "body_bytes": body.len(),
                "heads": document.get_heads().iter().map(ToString::to_string).collect::<Vec<_>>(),
            })
        });
        eprintln!(
            "mounted publisher after {elapsed:?}: pending={} document={document:?}",
            publisher.peer().pending_count()
        );
    }
    drop(publisher);
    eprintln!(
        "mounted governor after {elapsed:?}: {:?}",
        committed_snapshot(source)
    );
    eprintln!(
        "mounted snapshots: {:?}",
        mounted
            .iter()
            .map(|app| serde_json::json!({
                "connected": app.code.app.collaboration_is_connected(),
                "owner_phase": app.code.app.controlled_document_merge_phase_for_test(),
                "ui": app.code.app.controlled_native_input_snapshot_for_test(),
                "native": committed_snapshot(&app.network.native_owner().unwrap()),
                "publication": publication_snapshot(&app.network.native_owner().unwrap()),
            }))
            .collect::<Vec<_>>()
    );
}

fn publication_snapshot(owner: &NativeWorkspaceOwner) -> serde_json::Value {
    let room = RoomId::parse(tilo_joined_room::testing::ROOM).unwrap();
    let now = SystemTime::now();
    let status = owner
        .outgoing_status(room, now)
        .map(|status| serde_json::json!({ "pending": status.pending, "blocked": status.blocked }));
    let eligible = owner.with_outgoing(room, now, |view, outgoing| {
        Ok(serde_json::json!({
            "generation": view.generation(),
            "ids": outgoing.entries().map(|entry| hex::encode(entry.id())).collect::<Vec<_>>(),
        }))
    });
    serde_json::json!({ "status": status, "eligible": eligible })
}

fn committed_snapshot(owner: &NativeWorkspaceOwner) -> Option<serde_json::Value> {
    owner
        .with_current(
            RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            |view| {
                Ok(view.peer().document(APP, cold_code::RECORD).map(|bytes| {
                    let mut document = automerge::AutoCommit::load(&bytes).unwrap();
                    let body = cold_code::body(&document);
                    serde_json::json!({
                        "generation": view.generation(),
                        "missing_dependencies": document.get_missing_deps(&[]).len(),
                        "causally_complete": view.peer().plaintext_complete_heads(APP, cold_code::RECORD).is_some(),
                        "body_hash": hex::encode(Sha256::digest(body.as_bytes())),
                        "body_bytes": body.len(),
                        "heads": document.get_heads().iter().map(ToString::to_string).collect::<Vec<_>>(),
                    })
                }))
            },
        )
        .unwrap()
}

/// The project's terminal claims a pane, `run` queues a command line for it
/// through the mounted app's door, and the native owner commits the queued
/// request: the routed document job reached the app that owns the document
/// and applied there. The terminal running in that pane then answers the
/// run, and the request settles with its answer.
#[expect(
    clippy::future_not_send,
    reason = "mounted UI roots use Rc and run on this test runtime thread"
)]
async fn run_and_settle(mounted: &mut Mounted) {
    const PANE: &str = "5d4f0a52-6a3e-4c1e-9d5b-2f1d6c0b7e11";
    mounted.invoke_where("claim-pane", Some(PANE), |_| true);
    mounted.invoke_where("run", Some("echo settled"), |_| true);
    let runs = mounted
        .code
        .app
        .with_component_root(|root| {
            tilo_app::framework::with_surface::<code_editor::Project, code_editor::CodeApp, _>(
                root.as_any_mut()?,
                |code| code.terminal_runs(),
            )
        })
        .flatten()
        .expect("the mounted Code app");
    // This device's terminal runs the session in the claimed pane.
    runs.set_pane(PANE);
    let asked = tilo_test_env::within("the mounted app applies the routed run", async {
        loop {
            mounted.frame();
            if let Some(asked) = runs.take().pop() {
                break asked;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert_eq!(asked.command, "echo settled");
    tilo_test_env::within("the native owner commits the queued run", async {
        loop {
            mounted.frame();
            if runs_of(&mounted.network.native_owner().unwrap()) == (1, 0) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    asked.reply.finish(Ok(tilo_actions::ActionOutput {
        value: serde_json::to_value(terminal::resource::RunAnswer {
            run: Some(asked.request.clone()),
            exit: Some(0),
        })
        .ok(),
        resources: Vec::new(),
    }));
    tilo_test_env::within("the run settles with its answer", async {
        loop {
            mounted.frame();
            if runs_of(&mounted.network.native_owner().unwrap()) == (1, 1) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
}

/// How many command lines the committed project has queued for its terminal,
/// and how many of them settled.
fn runs_of(owner: &NativeWorkspaceOwner) -> (usize, usize) {
    owner
        .with_current(
            RoomId::parse(tilo_joined_room::testing::ROOM).unwrap(),
            |view| {
                Ok(view
                    .peer()
                    .document(APP, cold_code::RECORD)
                    .map_or((0, 0), |bytes| {
                        let document = automerge::AutoCommit::load(&bytes).unwrap();
                        <code_editor::Project as tilo_resource::Resource>::read(&document).map_or(
                            (0, 0),
                            |project| {
                                let requests = &project.terminal_requests;
                                let settled = requests
                                    .entries()
                                    .filter(|(id, _)| requests.outcome(id).is_some())
                                    .count();
                                (requests.entries().len(), settled)
                            },
                        )
                    }))
            },
        )
        .unwrap()
}
