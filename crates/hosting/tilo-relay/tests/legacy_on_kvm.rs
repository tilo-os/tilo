//! A legacy app launched from a member's relay environment, run in a real
//! legacy VM on a KVM worker: the launch goes from the guest's broker
//! socket through the relay to the worker, which boots the member's VM,
//! shows the app on its headless Tilo shell, and streams the window to the
//! member as H.264; the member's keystrokes reach the app.
//!
//! Needs KVM, the guest disk and the binaries, so it runs only where CI's
//! KVM job names them:
//!
//! - `TILO_LEGACY_E2E_IMAGE`: the guest qcow2
//! - `TILO_LEGACY_E2E_TILO_LEGACY`: `tilo-legacy`
//! - `TILO_LEGACY_E2E_SHELL`: a `shell` built with `agent-control`

#![allow(
    clippy::significant_drop_tightening,
    reason = "the relay, worker and member's streams deliberately live until the test ends"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use tilo_relay::environment_host::MemberArea;
use tilo_relay::legacy_vm_host::TiloLegacyHost;
use tilo_relay::{Relay, RelayConfig, TrustPolicy, WorkspaceBinding};
use tilo_relay_client::{open_legacy_windows, open_remote_environment};
use tilo_relay_protocol::config::LegacyAppsConfig;
use tilo_relay_protocol::environment::{MemberIdentity, WindowFrame, WindowInput};
use tilo_relay_protocol::legacy_window::{CHUNK_FIRST, CHUNK_KEY, Frame, InputEvent};
use tilo_user_identity::{DeviceCapabilities, RevocationList};
use ush::legacy_relay::{LaunchReply, LaunchRequest};

use crate::common::TestUser;
use crate::environment_sessions::{SIZE, WORKSPACE, record, remote, spec, start_worker};

const TYPED: &str = "hello-legacy";

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// Linux evdev codes for the characters the test types.
fn evdev(ch: char) -> u32 {
    match ch {
        'h' => 35,
        'e' => 18,
        'l' => 38,
        'o' => 24,
        'g' => 34,
        'a' => 30,
        'c' => 46,
        'y' => 21,
        '-' => 12,
        other => panic!("no evdev code for {other:?} in this test"),
    }
}

fn key(code: u32, pressed: bool) -> InputEvent {
    InputEvent::Key {
        keysym: code,
        pressed,
        modifiers: 0,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "needs KVM, the legacy guest disk and the binaries; CI's legacy VM job runs it"]
#[expect(
    clippy::too_many_lines,
    reason = "one member journey that reads top to bottom"
)]
async fn a_relay_launched_legacy_app_streams_from_a_kvm_guest() {
    let (Some(image), Some(tilo_legacy), Some(shell)) = (
        env_path("TILO_LEGACY_E2E_IMAGE"),
        env_path("TILO_LEGACY_E2E_TILO_LEGACY"),
        env_path("TILO_LEGACY_E2E_SHELL"),
    ) else {
        panic!("TILO_LEGACY_E2E_IMAGE, _TILO_LEGACY and _SHELL name the guest and binaries");
    };
    let provider = TestUser::fresh("Tilo hosting");
    let alice = TestUser::fresh("Alice");
    let relay_device = provider.enrol("relay", DeviceCapabilities::host());
    let worker_device = provider.enrol("worker", DeviceCapabilities::host());
    let alice_laptop = alice.enrol("alice-laptop", DeviceCapabilities::primary());
    let state = tempfile::tempdir().unwrap();
    // Short: the member's VM keeps Unix sockets several levels below it.
    let worker_root = tempfile::Builder::new()
        .prefix("lw")
        .tempdir_in("/var/tmp")
        .unwrap();
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
            vec![worker_device.device_pubkey(), alice_laptop.device_pubkey()],
            state.path().to_path_buf(),
        )
        .await
        .unwrap();

    let host = TiloLegacyHost::new(LegacyAppsConfig {
        tilo_legacy_binary: tilo_legacy,
        shell_binary: shell,
        guest_image: Some(image),
        idle_suspend_secs: Some(600),
    });
    let worker = start_worker(
        &relay,
        &provider,
        &worker_device,
        worker_root.path(),
        Arc::new(host.clone()),
    )
    .await;

    // Alice has an environment session on the worker, and watches her
    // legacy windows from her laptop.
    let laptop = remote(&relay, &alice_laptop).await;
    let session = open_remote_environment(&laptop, &record(), &spec(), SIZE, None)
        .await
        .unwrap();
    let mut windows = open_legacy_windows(&laptop, WORKSPACE).await.unwrap();

    // Her guest's broker asks the worker on the socket the worker made for
    // her supervisor; the worker routes the launch through the relay.
    let member = MemberIdentity {
        user: alice.user_pk,
        device: alice_laptop.device_pubkey(),
    };
    let socket = worker_root.path().join("alice-legacy.sock");
    let route = worker.environments.legacy_route.clone().unwrap();
    let _listening = route.listen(member, &socket, None).unwrap();
    let reply = tokio::task::spawn_blocking(move || {
        ush::legacy_relay::request(
            &socket,
            &LaunchRequest {
                app: "foot".into(),
                args: vec![
                    "--app-id".into(),
                    "foot".into(),
                    "sh".into(),
                    "-c".into(),
                    "echo legacy window ready; read line; echo \"$line\" > /tmp/typed".into(),
                ],
                cwd: "/home/alice".into(),
            },
        )
    })
    .await
    .unwrap()
    .unwrap();
    let LaunchReply::Launched { .. } = reply else {
        panic!("the launch was refused: {reply:?}");
    };

    // The VM boots (or wakes), the window maps on the worker's headless
    // shell, and its first keyframe reaches Alice.
    let (launch, surface) = tokio::time::timeout(Duration::from_secs(600), async {
        let mut launch = None;
        let mut surface = None;
        loop {
            match windows.next().await.unwrap().unwrap() {
                WindowFrame::Launched {
                    launch: id,
                    session: from,
                    app,
                } => {
                    assert_eq!(from, session.session());
                    assert_eq!(app, "foot");
                    launch = Some(id);
                }
                WindowFrame::Window { frame, .. } => match Frame::decode(&frame).unwrap() {
                    Frame::Start(stream) => {
                        assert_eq!(stream.app, "legacy.linux.foot");
                        assert_eq!(stream.codec, "h264");
                        assert!(stream.width > 100 && stream.height > 100, "{stream:?}");
                        surface = Some(u64::from_str_radix(&stream.surface, 16).unwrap());
                    }
                    Frame::Chunk { flags, total, .. }
                        if flags & CHUNK_KEY != 0 && flags & CHUNK_FIRST != 0 =>
                    {
                        assert!(total > 500, "a keyframe of a real window: {total} bytes");
                        break (launch.unwrap(), surface.unwrap());
                    }
                    _ => {}
                },
                WindowFrame::Exited { code, .. } => panic!("the app ended early: {code:?}"),
                WindowFrame::Watching => {}
            }
        }
    })
    .await
    .expect("the legacy window's first keyframe to reach the member");

    // Alice types a line; the app in the guest reads it and exits.
    let mut events = Vec::new();
    for ch in TYPED.chars() {
        events.push(key(evdev(ch), true));
        events.push(key(evdev(ch), false));
    }
    events.push(key(28, true));
    events.push(key(28, false));
    for batch in events.chunks(16) {
        windows
            .send_input(&WindowInput {
                launch,
                surface,
                events: batch.to_vec(),
            })
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if let WindowFrame::Exited {
                launch: ended,
                code,
            } = windows.next().await.unwrap().unwrap()
            {
                assert_eq!(ended, launch);
                assert_eq!(code, Some(0));
                break;
            }
        }
    })
    .await
    .expect("the app to exit after reading the line");

    // What she typed is in the guest.
    let area = MemberArea::of(worker_root.path(), &alice.user_pk);
    let typed = host
        .tilo_legacy(&area)
        .args(["run", "--no-window", "--", "cat", "/tmp/typed"])
        .output()
        .unwrap();
    assert_eq!(String::from_utf8_lossy(&typed.stdout).trim(), TYPED);
    let _ = host.tilo_legacy(&area).arg("stop").output();
    worker.serving.abort();
}
