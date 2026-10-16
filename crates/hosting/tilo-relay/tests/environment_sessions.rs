//! Members' environment sessions through an in-process relay and worker:
//! open, detach, re-attach from another device, tenancy, and a legacy
//! launch whose windows reach the member.

#![allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tilo_clock::unix_now_ms;
use tilo_relay::environment_host::{
    EnvironmentHost, EnvironmentHostConfig, Guest, GuestControl, GuestLauncher, GuestRequest,
    LaunchError, MemberContext, MemberQuota, NoMetering,
};
use tilo_relay::legacy_host::{LegacyApp, LegacyVmHost, LegacyWorker, request_legacy_launch};
use tilo_relay::{
    ComputeAccelerator, ComputeClient, ComputeEnvelope, ComputeWorkerAdvertisement,
    PowerAvailability, Relay, RelayConfig, TrustPolicy, WorkerEnvironments, WorkspaceBinding,
    serve_worker_stream,
};
use tilo_relay_client::{
    EnvironmentClientError, PtyEvent, RemoteRelay, attach_remote_environment, open_legacy_windows,
    open_remote_environment,
};
use tilo_relay_protocol::environment::{
    EnvironmentRecordRef, EnvironmentSpec, HomePolicy, LegacyFrame, LegacyGuestOs, LegacyLaunch,
    LegacyLaunchOutcome, PtySize, Refusal, WindowFrame, WindowInput,
};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

use crate::common::{TestDevice, TestUser, client_endpoint};

pub(crate) const WORKSPACE: u64 = 77;

/// A guest that prints a prompt, then echoes.
pub(crate) struct EchoLauncher;

struct EchoControl(Mutex<Option<oneshot::Sender<()>>>);

impl GuestControl for EchoControl {
    fn resize(&self, _size: PtySize) -> std::io::Result<()> {
        Ok(())
    }

    fn terminate(&self) {
        let stop = self.0.lock().unwrap().take();
        if let Some(stop) = stop {
            let _ = stop.send(());
        }
    }

    fn exit_code(&self) -> Option<i32> {
        Some(0)
    }
}

impl GuestLauncher for EchoLauncher {
    fn launch(
        &self,
        _member: &MemberContext,
        _request: &GuestRequest,
    ) -> Result<Guest, LaunchError> {
        let (host_side, mut guest_side) = tokio::io::duplex(1 << 16);
        let (output, input) = tokio::io::split(host_side);
        let (stop, mut stopped) = oneshot::channel();
        tokio::spawn(async move {
            let _ = guest_side.write_all(b"$ ").await;
            let mut buffer = [0; 1024];
            loop {
                tokio::select! {
                    _ = &mut stopped => break,
                    read = guest_side.read(&mut buffer) => match read {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if guest_side.write_all(&buffer[..read]).await.is_err() {
                                break;
                            }
                        }
                    },
                }
            }
        });
        Ok(Guest {
            output: Box::pin(output),
            input: Box::pin(input),
            control: Box::new(EchoControl(Mutex::new(Some(stop)))),
        })
    }
}

/// A legacy VM that opens one window and echoes input as window frames.
struct WindowVm;

impl LegacyVmHost for WindowVm {
    fn launch(
        &self,
        _member: &MemberContext,
        _launch: [u8; 16],
        _request: &LegacyLaunch,
    ) -> Result<LegacyApp, LaunchError> {
        let (frames_tx, frames) = mpsc::channel(8);
        let (input, mut input_rx) = mpsc::channel::<WindowInput>(8);
        tokio::spawn(async move {
            let _ = frames_tx.send(LegacyFrame::Started).await;
            let _ = frames_tx.send(LegacyFrame::Window(vec![0xF1, 1])).await;
            if let Some(event) = input_rx.recv().await {
                let _ = frames_tx
                    .send(LegacyFrame::Window(vec![
                        0xF1,
                        u8::try_from(event.surface).unwrap(),
                    ]))
                    .await;
                let _ = frames_tx.send(LegacyFrame::Exited { code: Some(0) }).await;
            }
        });
        Ok(LegacyApp { frames, input })
    }
}

pub(crate) fn spec() -> EnvironmentSpec {
    EnvironmentSpec {
        name: "dev".into(),
        runtime: tilo_relay_protocol::environment::RuntimeImages {
            release: "2026.10".into(),
            images: [(
                tilo_relay_protocol::environment::this_architecture().to_owned(),
                "tapp:01".to_owned(),
            )]
            .into(),
        },
        tools: Vec::new(),
        grants: Vec::new(),
        home: HomePolicy::Persistent,
    }
}

pub(crate) fn record() -> EnvironmentRecordRef {
    EnvironmentRecordRef {
        workspace_id: WORKSPACE,
        record_id: "env-1".into(),
    }
}

pub(crate) const SIZE: PtySize = PtySize { cols: 80, rows: 24 };

pub(crate) async fn remote(relay: &Relay, device: &TestDevice) -> RemoteRelay {
    RemoteRelay {
        endpoint: client_endpoint().await,
        address: relay.node_addr(),
        relay_user: None,
        cert: device.cert.clone(),
        signing_key: device.sk.clone(),
        revocations: RevocationList::new(),
    }
}

/// Read output until `expected` arrived; returns everything read.
pub(crate) async fn expect_output(
    stream: &mut tilo_relay_client::PtyStream,
    expected: &[u8],
) -> Vec<u8> {
    tilo_test_env::within("the expected environment output", async {
        let mut seen = Vec::new();
        while !seen.ends_with(expected) {
            match stream.next().await.unwrap() {
                Some(PtyEvent::Output(bytes)) => seen.extend(bytes),
                other => panic!("expected output, got {other:?}"),
            }
        }
        seen
    })
    .await
}

pub(crate) struct Worker {
    pub(crate) client: ComputeClient,
    pub(crate) serving: tokio::task::JoinHandle<()>,
    /// Read for the legacy launch route, which exists only on Linux.
    #[cfg(target_os = "linux")]
    pub(crate) environments: WorkerEnvironments,
}

/// Attach the provider's worker, advertise environments and KVM, and serve
/// every stream the relay opens to it.
pub(crate) async fn start_worker(
    relay: &Relay,
    provider: &TestUser,
    worker: &TestDevice,
    root: &std::path::Path,
    legacy: Arc<dyn LegacyVmHost>,
) -> Worker {
    start_worker_with(relay, provider, worker, root, WORKSPACE, legacy).await
}

/// [`start_worker`] for the workspace `workspace`.
#[cfg(target_os = "linux")]
pub(crate) async fn start_worker_in(
    relay: &Relay,
    provider: &TestUser,
    worker: &TestDevice,
    root: &std::path::Path,
    workspace: u64,
) -> Worker {
    start_worker_with(relay, provider, worker, root, workspace, Arc::new(WindowVm)).await
}

/// [`start_worker`] for `workspace`, running legacy apps with `legacy`.
pub(crate) async fn start_worker_with(
    relay: &Relay,
    provider: &TestUser,
    worker: &TestDevice,
    root: &std::path::Path,
    workspace: u64,
    legacy: Arc<dyn LegacyVmHost>,
) -> Worker {
    let mut client = ComputeClient::connect(
        client_endpoint().await,
        relay.node_addr(),
        provider.user_pk,
        None,
        worker.cert.clone(),
        worker.sk.clone(),
        RevocationList::new(),
    )
    .await
    .unwrap();
    client.attach(workspace).await.unwrap();
    assert!(matches!(
        client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let issued_at_ms = unix_now_ms();
    client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: workspace,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker.device_pubkey(),
                workspace_id: workspace,
                apps: BTreeSet::new(),
                accelerators: BTreeSet::from([ComputeAccelerator::Kvm]),
                assignment_capacity: 1,
                active_assignments: 0,
                environment_capacity: 4,
                active_environments: 0,
                power: PowerAvailability::External,
                energy_saver: false,
                issued_at_ms,
                expires_at_ms: issued_at_ms.saturating_add(60_000),
            },
        })
        .await
        .unwrap();
    // A ping answered after the advertisement means it was applied.
    client.send(&ComputeEnvelope::Ping).await.unwrap();
    assert!(matches!(
        client.receive().await.unwrap().envelope,
        ComputeEnvelope::Pong
    ));
    let environments = WorkerEnvironments {
        host: Some(EnvironmentHost::new(
            EnvironmentHostConfig {
                root: root.to_path_buf(),
                quota: MemberQuota::default(),
                idle_timeout: Duration::from_secs(3600),
                capacity: 4,
            },
            Arc::new(EchoLauncher),
            Arc::new(NoMetering),
        )),
        legacy: Some(LegacyWorker::new(
            legacy,
            root.to_path_buf(),
            MemberQuota::default(),
        )),
        // Guests' own legacy launches reach the relay through the route, as
        // on a production worker; like `ush`, it exists only on Linux.
        #[cfg(target_os = "linux")]
        legacy_route: Some(tilo_relay::legacy_route::LegacyLaunchRoute::new()),
    };
    #[cfg(target_os = "linux")]
    if let (Some(route), Some(host)) = (&environments.legacy_route, &environments.host) {
        route.set_host(host.downgrade());
        route.set_relay(client.connection());
    }
    let served = environments.clone();
    let connection = client.connection();
    let serving = tokio::spawn(async move {
        while let Ok((mut send, mut receive)) = connection.accept_bi().await {
            let environments = served.clone();
            tokio::spawn(async move {
                let _ = serve_worker_stream(environments, &mut send, &mut receive).await;
                let _ = send.finish();
            });
        }
    });
    Worker {
        client,
        serving,
        #[cfg(target_os = "linux")]
        environments,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one member journey that reads top to bottom"
)]
async fn members_run_reattach_and_launch_legacy_apps_through_a_relay_worker() {
    let provider = TestUser::fresh("Tilo hosting");
    let alice = TestUser::fresh("Alice");
    let bob = TestUser::fresh("Bob");
    let carol = TestUser::fresh("Carol");
    let relay_device = provider.enrol("relay", DeviceCapabilities::host());
    let worker_device = provider.enrol("worker", DeviceCapabilities::host());
    let alice_laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let alice_phone = alice.enrol("alice-phone", DeviceCapabilities::primary());
    let bob_laptop = bob.enrol("bob-laptop", DeviceCapabilities::primary());
    let carol_laptop = carol.enrol("carol-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            trust_policy: TrustPolicy::AllowAny,
            workspaces: vec![WorkspaceBinding {
                id: WORKSPACE,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: relay_device.sk.clone(),
            device_cert: relay_device.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            WORKSPACE,
            true,
            vec![
                worker_device.device_pubkey(),
                alice_laptop.device_pubkey(),
                alice_phone.device_pubkey(),
                bob_laptop.device_pubkey(),
            ],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();

    // Before any worker advertises, there is nowhere to run.
    let laptop = remote(&relay, &alice_laptop).await;
    let early = open_remote_environment(&laptop, &record(), &spec(), SIZE, None).await;
    assert!(matches!(
        early,
        Err(EnvironmentClientError::Refused {
            reason: Refusal::NoWorker,
            ..
        })
    ));

    let worker = start_worker(
        &relay,
        &provider,
        &worker_device,
        worker_root.path(),
        Arc::new(WindowVm),
    )
    .await;

    // Alice opens a session, types, and walks away.
    let mut first = open_remote_environment(&laptop, &record(), &spec(), SIZE, None)
        .await
        .unwrap();
    assert!(!first.resumed());
    let session = first.session();
    expect_output(&mut first, b"$ ").await;
    first.write(b"make test\n").await.unwrap();
    expect_output(&mut first, b"make test\n").await;
    drop(first);

    // From her phone she re-attaches and sees what happened.
    let phone = remote(&relay, &alice_phone).await;
    let mut second = attach_remote_environment(&phone, WORKSPACE, session, SIZE)
        .await
        .unwrap();
    assert!(second.resumed());
    assert_eq!(second.take_scrollback(), b"$ make test\n");
    second.write(b"ls\n").await.unwrap();
    expect_output(&mut second, b"ls\n").await;

    // Bob is a member of the workspace but not the session's owner; Carol
    // is not a member at all.
    let bobs =
        attach_remote_environment(&remote(&relay, &bob_laptop).await, WORKSPACE, session, SIZE)
            .await;
    assert!(matches!(
        bobs,
        Err(EnvironmentClientError::Refused {
            reason: Refusal::NotOwner,
            ..
        })
    ));
    let carols = open_remote_environment(
        &remote(&relay, &carol_laptop).await,
        &record(),
        &spec(),
        SIZE,
        None,
    )
    .await;
    assert!(matches!(
        carols,
        Err(EnvironmentClientError::Refused {
            reason: Refusal::NotAMember,
            ..
        })
    ));

    // A legacy launch from Alice's session reaches her window stream, and
    // her input reaches the app.
    let mut windows = open_legacy_windows(&phone, WORKSPACE).await.unwrap();
    let outcome = tilo_test_env::within("the legacy launch to be routed", async {
        loop {
            let outcome = request_legacy_launch(
                &worker.client.connection(),
                WORKSPACE,
                session,
                LegacyLaunch {
                    guest: LegacyGuestOs::Linux,
                    app: "code".into(),
                    args: vec![".".into()],
                    cwd: "/home/alice".into(),
                },
            )
            .await
            .unwrap();
            if matches!(outcome, LegacyLaunchOutcome::Launched { .. }) {
                return outcome;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let LegacyLaunchOutcome::Launched { launch } = outcome else {
        unreachable!()
    };
    let mut saw = Vec::new();
    tilo_test_env::within("the legacy window to reach the member", async {
        loop {
            let frame = windows.next().await.unwrap().unwrap();
            let window = matches!(&frame, WindowFrame::Window { .. });
            saw.push(frame);
            if window {
                break;
            }
        }
    })
    .await;
    assert!(saw.contains(&WindowFrame::Launched {
        launch,
        session,
        app: "code".into(),
    }));
    windows
        .send_input(&WindowInput {
            launch,
            surface: 7,
            events: vec![
                tilo_relay_protocol::legacy_window::InputEvent::PointerAxis { dx: 0.0, dy: 1.0 },
            ],
        })
        .await
        .unwrap();
    tilo_test_env::within("the input's echo and the app's exit", async {
        let mut echoed = false;
        loop {
            match windows.next().await.unwrap().unwrap() {
                WindowFrame::Window { frame, .. } if frame == vec![0xF1, 7] => echoed = true,
                WindowFrame::Exited {
                    launch: ended,
                    code,
                } => {
                    assert_eq!(ended, launch);
                    assert_eq!(code, Some(0));
                    break;
                }
                _ => {}
            }
        }
        assert!(echoed);
    })
    .await;

    // Only the worker hosting a session may launch from it.
    let forged = request_legacy_launch(
        &worker.client.connection(),
        WORKSPACE,
        [0xEE; 16],
        LegacyLaunch {
            guest: LegacyGuestOs::Linux,
            app: "code".into(),
            args: Vec::new(),
            cwd: "/".into(),
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        forged,
        LegacyLaunchOutcome::Refused {
            reason: Refusal::UnknownSession,
            ..
        }
    ));

    // Ending the session ends it everywhere.
    second.terminate().await.unwrap();
    tilo_test_env::within("the session to exit", async {
        loop {
            match second.next().await.unwrap() {
                Some(PtyEvent::Exited(code)) => {
                    assert_eq!(code, Some(0));
                    break;
                }
                Some(_) => {}
                None => panic!("the stream ended without an exit"),
            }
        }
    })
    .await;
    let gone = tilo_test_env::within("the ended session to be forgotten", async {
        loop {
            match attach_remote_environment(&laptop, WORKSPACE, session, SIZE).await {
                Err(EnvironmentClientError::Refused { reason, .. }) => return reason,
                Ok(stream) => drop(stream),
                Err(other) => panic!("{other}"),
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(gone, Refusal::UnknownSession);
    worker.serving.abort();
}

/// The relay's management status says what its workers run for
/// environments, as they come and go: what a device stores in its
/// home-peer registration and its Terminal filters relays by.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_relay_says_whether_its_workers_run_environments() {
    let provider = TestUser::fresh("Tilo hosting");
    let relay_device = provider.enrol("relay", DeviceCapabilities::host());
    let relay_key = relay_device.device_pubkey();
    let worker_device = provider.enrol("worker", DeviceCapabilities::host());
    let owner_laptop = provider.enrol("laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding {
                id: WORKSPACE,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: provider.user_pk,
            device_signing_key: relay_device.sk.clone(),
            device_cert: relay_device.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            WORKSPACE,
            true,
            vec![worker_device.device_pubkey(), owner_laptop.device_pubkey()],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();
    let endpoint = client_endpoint().await;
    let status = || async {
        tilo_relay::client::RelayClient::manage_home_peer(
            &endpoint,
            relay.node_addr(),
            provider.user_pk,
            relay_key,
            owner_laptop.cert.clone(),
            owner_laptop.sk.clone(),
            RevocationList::new(),
            tilo_relay::transport::HomePeerManagementRequest::Status,
        )
        .await
        .unwrap()
        .environments
    };

    assert_eq!(
        status().await,
        tilo_relay_protocol::RelayEnvironments::default()
    );
    let worker = start_worker(
        &relay,
        &provider,
        &worker_device,
        worker_root.path(),
        Arc::new(WindowVm),
    )
    .await;
    assert_eq!(
        status().await,
        tilo_relay_protocol::RelayEnvironments {
            sessions: true,
            legacy_apps: true,
        }
    );
    worker.serving.abort();
    worker.client.close().await;
    tilo_test_env::within("the departed worker to be withdrawn", async {
        while status().await != tilo_relay_protocol::RelayEnvironments::default() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
}

/// A device asks its relays what they run, within a budget, and Terminal's
/// relay list follows the answers: a relay without a worker is not offered,
/// and one that does not answer does not hold the others up.
///
/// Linux only: Terminal's relay list (`ush::relay`) lives in the `ush`
/// supervisor, which the relay links only on Linux.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[expect(
    clippy::too_many_lines,
    reason = "one device's view of its relays as a worker comes, read top to bottom"
)]
async fn a_device_asks_its_relays_and_offers_only_those_with_workers() {
    use tilo_user_identity::{IdentityPaths, persist};

    let owner = TestUser::fresh("Owner");
    let relay_device = owner.enrol("relay", DeviceCapabilities::host());
    let worker_device = owner.enrol("worker", DeviceCapabilities::host());
    let laptop = owner.enrol("laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    let worker_root = tempfile::tempdir().unwrap();
    let relay = Relay::start(
        RelayConfig {
            state_root: state.path().to_path_buf(),
            workspaces: vec![WorkspaceBinding {
                id: WORKSPACE,
                pin: true,
                replication: tilo_relay::ReplicationIntent::FullWorkspace,
                admitted_users: Vec::new(),
                shell_authority: None,
                checkpoint_prefix_recovery: tilo_relay::CheckpointPrefixRecovery::Disabled,
            }],
            ..RelayConfig::default()
        },
        tilo_relay::daemon::RelayIdentity {
            user_public_key: owner.user_pk,
            device_signing_key: relay_device.sk.clone(),
            device_cert: relay_device.cert.clone(),
            revocations: RevocationList::new(),
        },
    )
    .await
    .unwrap();
    relay
        .set_workspace_membership(
            WORKSPACE,
            true,
            vec![worker_device.device_pubkey(), laptop.device_pubkey()],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();

    // The laptop's identity, and two relays registered: this one, and one
    // that never answers.
    let identity = tempfile::tempdir().unwrap();
    let paths = IdentityPaths::under(identity.path());
    for file in [
        &paths.user_pubkey,
        &paths.device_privkey,
        &paths.device_cert,
    ] {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    }
    persist::write_public_key(&paths.user_pubkey, &owner.user_pk).unwrap();
    persist::write_device_key(&paths.device_privkey, &laptop.sk).unwrap();
    persist::write_device_cert(&paths.device_cert, &laptop.cert).unwrap();
    let personal = tilo_relay_protocol::personal_continuity_workspace_id(&owner.user_pk);
    let address = relay.node_addr();
    let silent = iroh_base::SecretKey::from_bytes(&[9; 32]).public();
    std::fs::write(
        identity.path().join("home-peers.json"),
        serde_json::json!([
            {"schema_version": 3,
             "device_node_id": hex::encode(relay_device.device_pubkey()),
             "relay_service_node_id": hex::encode(address.id.as_bytes()),
             "relay_endpoint": address, "label": "Home",
             "user_public_key": hex::encode(owner.user_pk),
             "relay_workspace_ids": [personal]},
            {"schema_version": 3,
             "device_node_id": hex::encode([8u8; 32]),
             "relay_service_node_id": hex::encode(silent.as_bytes()),
             "label": "Away",
             "user_public_key": hex::encode(owner.user_pk),
             "relay_workspace_ids": [personal]},
        ])
        .to_string(),
    )
    .unwrap();
    let offers = identity.path().join("cache/relay-offers.json");
    let budget = Duration::from_millis(1500);
    let registry = identity.path().join("home-peers.json");
    let offered = |offers: &tilo_relay_protocol::RelayOffers| -> Vec<String> {
        ush::relay::relays_with_offers(&registry, &owner.user_pk, offers)
            .into_iter()
            .filter(|peer| peer.runs_environments)
            .map(|peer| peer.label)
            .collect()
    };

    // No worker yet: asked, answered, not offered; the silent relay costs
    // the budget, not its own dial timeout. Unbudgeted, that ask waits out
    // QUIC's 30 s idle timeout, so a bound well under it still proves the
    // budget holds while leaving a loaded runner room for the endpoint's
    // bind and close around it.
    let started = std::time::Instant::now();
    let answered =
        tilo_relay::relay_offers::refresh_offers(identity.path(), &offers, budget, 1_000)
            .await
            .unwrap();
    let took = started.elapsed();
    assert!(
        took < budget + Duration::from_secs(10),
        "the silent relay held the ask for {took:?}, past its {budget:?} budget"
    );
    assert_eq!(answered.len(), 2, "every relay was asked");
    assert!(answered.values().all(|offer| offer.checked_at_ms == 1_000));
    assert!(offered(&answered).is_empty());

    // A worker attaches: the next ask offers the relay that has it.
    let worker = start_worker(
        &relay,
        &owner,
        &worker_device,
        worker_root.path(),
        Arc::new(WindowVm),
    )
    .await;
    let answered =
        tilo_relay::relay_offers::refresh_offers(identity.path(), &offers, budget, 2_000)
            .await
            .unwrap();
    assert_eq!(offered(&answered), vec!["Home".to_owned()]);
    assert_eq!(
        tilo_relay::relay_offers::load_offers(&offers),
        answered,
        "what the picker reads"
    );
    worker.serving.abort();
    worker.client.close().await;
}
