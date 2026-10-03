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

const WORKSPACE: u64 = 77;

/// A guest that prints a prompt, then echoes.
struct EchoLauncher;

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

fn spec() -> EnvironmentSpec {
    EnvironmentSpec {
        name: "dev".into(),
        runtime: "so.tilo.env.fedora".into(),
        tools: Vec::new(),
        grants: Vec::new(),
        home: HomePolicy::Persistent,
    }
}

fn record() -> EnvironmentRecordRef {
    EnvironmentRecordRef {
        workspace_id: WORKSPACE,
        record_id: "env-1".into(),
    }
}

const SIZE: PtySize = PtySize { cols: 80, rows: 24 };

async fn remote(relay: &Relay, device: &TestDevice) -> RemoteRelay {
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
async fn expect_output(stream: &mut tilo_relay_client::PtyStream, expected: &[u8]) -> Vec<u8> {
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

struct Worker {
    client: ComputeClient,
    serving: tokio::task::JoinHandle<()>,
}

/// Attach the provider's worker, advertise environments and KVM, and serve
/// every stream the relay opens to it.
async fn start_worker(
    relay: &Relay,
    provider: &TestUser,
    worker: &TestDevice,
    root: &std::path::Path,
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
    client.attach(WORKSPACE).await.unwrap();
    assert!(matches!(
        client.receive().await.unwrap().envelope,
        ComputeEnvelope::Ready { .. }
    ));
    let issued_at_ms = unix_now_ms();
    client
        .send(&ComputeEnvelope::Advertise {
            workspace_id: WORKSPACE,
            advertisement: ComputeWorkerAdvertisement {
                version: 1,
                worker: worker.device_pubkey(),
                workspace_id: WORKSPACE,
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
            Arc::new(WindowVm),
            root.to_path_buf(),
            MemberQuota::default(),
        )),
    };
    let connection = client.connection();
    let serving = tokio::spawn(async move {
        while let Ok((mut send, mut receive)) = connection.accept_bi().await {
            let environments = environments.clone();
            tokio::spawn(async move {
                let _ = serve_worker_stream(environments, &mut send, &mut receive).await;
                let _ = send.finish();
            });
        }
    });
    Worker { client, serving }
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

    let worker = start_worker(&relay, &provider, &worker_device, worker_root.path()).await;

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
