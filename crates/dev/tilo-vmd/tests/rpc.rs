//! Drives a real `tilo-vmd rpc` subprocess through the production client
//! over stdio pipes, as `tilo-vm` does over SSH, against a temporary state
//! root with the side-effect-free `record` apply backend.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use sha2::Digest as _;

use tilo_vm_protocol::{
    Action, Client, ClientError, ComponentInfo, Gc, LayerImport, LayerList, LayerSource, Owner,
    PROTOCOL_VERSION, Policy, Provenance, Rollback, Scope, SeatClaim, SeatRelease, Shot,
    SlotCreate, SlotKind, SlotList, SlotRm, SlotShow, Status, Swap, code,
};

struct Daemon {
    child: Child,
    client: Client<BufReader<ChildStdout>, ChildStdin>,
}

impl Daemon {
    fn spawn(root: &std::path::Path) -> Self {
        Self::spawn_with(root, "record")
    }

    fn spawn_with(root: &std::path::Path, apply: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-vmd"))
            .args([
                "rpc",
                "--apply",
                apply,
                "--manifest",
                "/nonexistent",
                "--base",
                "/nonexistent",
            ])
            .arg("--root")
            .arg(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        let stdin = child.stdin.take().unwrap();
        let mut client = Client::new(stdout, stdin);
        client.enable_binary_uploads();
        Self { child, client }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tarball(path: &str, data: &[u8]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(data.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder.append_data(&mut header, path, data).unwrap();
    builder.into_inner().unwrap()
}

fn import(
    client: &mut Client<BufReader<ChildStdout>, ChildStdin>,
    component: &str,
    bytes: &[u8],
) -> String {
    let upload = client.upload(bytes).unwrap();
    let mut provenance = Provenance::local(component);
    provenance.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
    client
        .call(LayerImport {
            source: LayerSource::Upload(upload),
            provenance: Box::new(provenance),
        })
        .unwrap()
        .hash
}

fn rpc_error<T: std::fmt::Debug>(result: Result<T, ClientError>) -> tilo_vm_protocol::RpcError {
    match result {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn slot_and_kernel_lifecycles_over_stdio() {
    // Phase `full_slot_lifecycle_over_stdio`.
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;

        // Everything but hello is refused before the handshake.
        let error = rpc_error(client.call(SlotList {}));
        assert_eq!(error.code, code::VERSION_MISMATCH);
        assert!(!error.hint.is_empty());

        let hello = client.hello("test").unwrap();
        assert_eq!(hello.protocol, PROTOCOL_VERSION);
        assert_eq!(hello.arch, std::env::consts::ARCH);

        let created = client
            .call(SlotCreate {
                name: "agent-a".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })
            .unwrap();
        assert!(created.created);
        assert!(created.actions.contains(&Action::StartUnit {
            unit: "tilo-vm-session@agent-a.service".into()
        }));
        let again = client
            .call(SlotCreate {
                name: "agent-a".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })
            .unwrap();
        assert!(!again.created, "create is idempotent");

        // A multi-chunk upload lands as one content-addressed layer.
        let big = vec![7_u8; tilo_vm_protocol::UPLOAD_CHUNK_BYTES * 2 + 17];
        let v1 = import(client, "shell", &tarball("usr/bin/shell", &big));
        assert_eq!(import(client, "shell", &tarball("usr/bin/shell", &big)), v1);
        let v2 = import(client, "shell", &tarball("usr/bin/shell", b"v2"));
        assert_eq!(client.call(LayerList {}).unwrap().len(), 2);

        let session = ComponentInfo::new(Scope::Session, vec![]);
        let swap = |layer: &str| Swap {
            slot: "agent-a".into(),
            component: "shell".into(),
            layer: layer.into(),
            info: Some(session.clone()),
        };
        let applied = client.call(swap(&v1)).unwrap();
        assert_eq!(
            (applied.generation, applied.policy, applied.changed),
            (1, Policy::SessionRestart, true)
        );
        assert!(applied.actions.contains(&Action::RestartUnit {
            unit: "user@60001.service".into()
        }));
        assert!(
            !client.call(swap(&v1)).unwrap().changed,
            "swap is idempotent"
        );
        assert_eq!(client.call(swap(&v2)).unwrap().generation, 2);

        let back = client
            .call(Rollback {
                slot: "agent-a".into(),
                to: None,
            })
            .unwrap();
        assert_eq!(back.generation, 1);
        let status = client
            .call(Status {
                slot: Some("agent-a".into()),
            })
            .unwrap();
        assert_eq!(status.slots[0].components["shell"], v1);
        // The base generation is listed too: rollback --to 0 reaches it.
        assert_eq!(status.slots[0].generations, [0, 1, 2]);
        assert!(status.issues.is_empty(), "{:?}", status.issues);

        // slot.show names each file the slot sees, its digest and its layer.
        let shown = client
            .call(SlotShow {
                name: "agent-a".into(),
            })
            .unwrap();
        assert_eq!(shown.slot.generation, 1);
        assert_eq!(shown.files.len(), 1, "{:?}", shown.files);
        let file = &shown.files[0];
        assert_eq!(
            (
                file.component.as_str(),
                file.path.as_str(),
                file.layer.as_str()
            ),
            ("shell", "/usr/bin/shell", v1.as_str())
        );
        let digest = hex::encode(sha2::Sha256::digest(&big));
        assert_eq!(file.sha256.as_deref(), Some(digest.as_str()));

        // VM-scope component into a user slot: refused, with the right redirect.
        let fs_layer = import(client, "fs", &tarball("usr/bin/fs", b"fs"));
        let error = rpc_error(client.call(Swap {
            slot: "agent-a".into(),
            component: "fs".into(),
            layer: fs_layer.clone(),
            info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
        }));
        assert_eq!(error.code, code::SCOPE);
        assert!(
            error.hint.contains("tilo-vm swap --slot vm fs"),
            "{}",
            error.hint
        );

        // Into the vm slot it refreshes sysext, restarts its units and every slot session.
        let vm = client
            .call(Swap {
                slot: "vm".into(),
                component: "fs".into(),
                layer: fs_layer,
                info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
            })
            .unwrap();
        assert_eq!(vm.policy, Policy::VmRefresh);
        assert!(vm.actions.contains(&Action::SysextRefresh));
        assert!(vm.actions.contains(&Action::RestartUnit {
            unit: "fs.service".into()
        }));
        assert!(vm.actions.contains(&Action::StartUnit {
            unit: "tilo-vm-session@agent-a.service".into()
        }));

        // seat0 is a lease: a second claimant is told who holds it, unless it steals.
        client
            .call(SlotCreate {
                name: "agent-b".into(),
                kind: SlotKind::Headful,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })
            .unwrap();
        let owner = Owner {
            host: Some("host-a".into()),
            worktree: Some("wt-a".into()),
            branch: None,
        };
        assert!(
            client
                .call(SeatClaim {
                    slot: "agent-a".into(),
                    owner,
                    steal: false
                })
                .unwrap()
                .changed
        );
        let error = rpc_error(client.call(SeatClaim {
            slot: "agent-b".into(),
            owner: Owner::default(),
            steal: false,
        }));
        assert!(
            error.message.contains("agent-a") && error.message.contains("host-a"),
            "{}",
            error.message
        );
        assert!(error.hint.contains("--steal"));
        let stolen = client
            .call(SeatClaim {
                slot: "agent-b".into(),
                owner: Owner::default(),
                steal: true,
            })
            .unwrap();
        assert!(stolen.changed);
        let released = client
            .call(SeatRelease {
                slot: "agent-b".into(),
            })
            .unwrap();
        assert_eq!(released.seat, "seat-agent-b");
        assert!(released.actions.contains(&Action::StartUnit {
            unit: "greetd.service".into()
        }));

        assert!(
            !client
                .call(Shot {
                    slot: "agent-a".into()
                })
                .unwrap()
                .png
                .is_empty()
        );

        // gc prunes generations beyond `keep` but never the current one; young
        // unreferenced layers survive because a swap may still be in flight.
        let dry = client
            .call(Gc {
                keep: 0,
                dry_run: true,
                keep_targets: None,
                free_goal_bytes: None,
                min_age_seconds: None,
            })
            .unwrap();
        assert_eq!(dry.removed_generations, ["agent-a/2"]);
        assert_eq!(
            client
                .call(Status {
                    slot: Some("agent-a".into())
                })
                .unwrap()
                .slots[0]
                .generations,
            [0, 1, 2]
        );
        let gc = client
            .call(Gc {
                keep: 0,
                dry_run: false,
                keep_targets: None,
                free_goal_bytes: None,
                min_age_seconds: None,
            })
            .unwrap();
        assert_eq!(gc.removed_generations, ["agent-a/2"]);
        assert!(gc.removed_layers.is_empty());
        assert!(gc.storage.layers_bytes > 0);
        // What stayed for its age is named, with when it becomes eligible.
        assert_eq!(gc.min_age_seconds, 3600);
        let kept = gc
            .kept_young
            .iter()
            .find(|k| k.hash == v2)
            .expect("v2 is unreferenced and young");
        assert_eq!(kept.component, "shell");
        assert!(kept.eligible_in_seconds > 0 && kept.eligible_in_seconds <= 3600);
        // A minimum age of 0 removes one's own orphaned layers at once.
        let now = client
            .call(Gc {
                keep: 0,
                dry_run: false,
                keep_targets: None,
                free_goal_bytes: None,
                min_age_seconds: Some(0),
            })
            .unwrap();
        assert!(now.removed_layers.contains(&v2), "{:?}", now.removed_layers);
        assert!(now.kept_young.is_empty());

        assert!(
            client
                .call(SlotRm {
                    name: "agent-a".into()
                })
                .unwrap()
                .removed
        );
        assert!(
            !client
                .call(SlotRm {
                    name: "agent-a".into()
                })
                .unwrap()
                .removed,
            "rm is idempotent"
        );
        assert_eq!(
            client.call(SlotList {}).unwrap().len(),
            2,
            "agent-b and vm remain"
        );
    }
    // Phase `a_kernel_swap_prepares_the_next_boot_and_rollback_undoes_it`.
    {
        use tilo_vm_protocol::BootConfirm;

        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client.hello("test").unwrap();

        let kernel = ComponentInfo {
            reboot: true,
            ..ComponentInfo::new(Scope::Vm, vec![])
        };
        let layer = import(
            client,
            "kernel",
            &tarball(
                "usr/lib/modules/7.2.5-200.fc44.x86_64+dev/vmlinuz",
                b"kernel",
            ),
        );
        let swapped = client
            .call(Swap {
                slot: "vm".into(),
                component: "kernel".into(),
                layer: layer.clone(),
                info: Some(kernel.clone()),
            })
            .unwrap();
        assert!(swapped.changed);
        assert_eq!(swapped.policy, Policy::Reboot);
        let boot = swapped
            .boot
            .clone()
            .expect("a kernel swap reports its boot entry");
        assert!(boot.kver.is_some());
        let entry = boot.entry.unwrap();
        assert!(entry.contains(&layer), "{entry}");
        assert!(swapped.actions.contains(&Action::SetNextBoot { entry }));
        // Nothing restarts before the reboot the client runs.
        assert!(
            !swapped
                .actions
                .iter()
                .any(|a| matches!(a, Action::RestartUnit { .. })),
            "{:?}",
            swapped.actions
        );

        let confirmed = client.call(BootConfirm::default()).unwrap();
        assert!(confirmed.confirmed);

        // The same layer again: nothing to boot.
        let again = client
            .call(Swap {
                slot: "vm".into(),
                component: "kernel".into(),
                layer,
                info: Some(kernel),
            })
            .unwrap();
        assert!(!again.changed && again.boot.is_none());

        // Back to the base generation: the image's kernel is the next boot.
        let back = client
            .call(Rollback {
                slot: "vm".into(),
                to: None,
            })
            .unwrap();
        assert_eq!(back.policy, Policy::Reboot);
        let boot = back
            .boot
            .expect("rolling a kernel back reports the boot entry");
        assert_eq!(boot.kver, None);
    }
}

#[test]
fn rpc_requests_are_validated_and_routed_per_slot() {
    // Phase `malformed_lines_get_error_responses`.
    {
        use std::io::{BufRead, Write};
        // Raw lines, bypassing the typed client.
        let root = tempfile::tempdir().unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-vmd"))
            .args(["rpc", "--apply", "record", "--root"])
            .arg(root.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = child.stdin.take().unwrap();
        let mut output = BufReader::new(child.stdout.take().unwrap());
        writeln!(input, "not json").unwrap();
        writeln!(
            input,
            r#"{{"jsonrpc":"2.0","id":5,"method":"no.such","params":{{}}}}"#
        )
        .unwrap();
        input.flush().unwrap();
        for expected_id in [0, 5] {
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            let value: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert_eq!(value["id"], expected_id);
            assert_eq!(value["error"]["code"], code::INVALID_PARAMS);
            assert!(!value["error"]["hint"].as_str().unwrap().is_empty());
        }
        drop(input);
        assert!(child.wait().unwrap().success());
    }
    // Phase `input_goes_to_headful_seats_and_apps_install_per_slot`.
    {
        use tilo_vm_protocol::{AppInstall, Input, InputAction};
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        for (name, kind) in [("a", SlotKind::Headless), ("b", SlotKind::Headful)] {
            client
                .call(SlotCreate {
                    name: name.into(),
                    kind,
                    owner: Owner::default(),
                    outputs: vec![],
                    identity: false,
                })
                .unwrap();
        }
        let keys = vec![InputAction::Key {
            combos: vec!["ctrl+alt+t".into()],
        }];
        let error = rpc_error(client.call(Input {
            slot: "a".into(),
            actions: keys.clone(),
        }));
        assert_eq!(error.code, code::INVALID);
        assert!(error.hint.contains("tilo-vm control"), "{}", error.hint);
        let sent = client
            .call(Input {
                slot: "b".into(),
                actions: keys,
            })
            .unwrap();
        assert_eq!(sent.seat, "seat-b");
        assert!(sent.events > 0);
        let error = rpc_error(client.call(Input {
            slot: "b".into(),
            actions: vec![InputAction::Key {
                combos: vec!["ctrl+nokey".into()],
            }],
        }));
        assert_eq!(error.code, code::INVALID_PARAMS);
        assert!(error.message.contains("nokey"), "{}", error.message);

        let upload = client.upload(&b"tapp bytes"[..]).unwrap();
        let installed = client
            .call(AppInstall {
                slot: "b".into(),
                source: LayerSource::Upload(upload),
            })
            .unwrap();
        assert_eq!(installed.slot, "b");
        assert!(installed.id.is_some_and(|id| id.starts_with("so.tilo.")));
        let upload = client.upload(&b"tapp bytes"[..]).unwrap();
        let error = rpc_error(client.call(AppInstall {
            slot: "vm".into(),
            source: LayerSource::Upload(upload),
        }));
        assert!(error.hint.contains("headful or headless"), "{}", error.hint);
    }
}

#[test]
fn device_login_input_uses_the_physical_seat() {
    use tilo_vm_protocol::{Input, InputAction};

    let actions = vec![InputAction::Key {
        combos: vec!["a".into()],
    }];
    let root = tempfile::tempdir().unwrap();
    let mut login = Daemon::spawn_with(root.path(), "record-login");
    login.client.hello("test").unwrap();
    let sent = login
        .client
        .call(Input {
            slot: "device".into(),
            actions: actions.clone(),
        })
        .unwrap();
    assert_eq!(sent.seat, "seat0");
    assert!(sent.events > 0);

    let board_root = tempfile::tempdir().unwrap();
    let mut board = Daemon::spawn_with(board_root.path(), "record-device");
    board.client.hello("test").unwrap();
    let error = rpc_error(board.client.call(Input {
        slot: "device".into(),
        actions,
    }));
    assert_eq!(error.code, code::INVALID);
    assert!(error.message.contains("headless"));
}

#[test]
fn the_device_slot_is_the_machines_own_session() {
    // Phase `a_machine_without_its_own_session_has_no_device_slot`.
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        let layer = import(client, "shell", &tarball("usr/bin/shell", b"v1"));
        let error = rpc_error(client.call(Swap {
            slot: "device".into(),
            component: "shell".into(),
            layer,
            info: Some(ComponentInfo::new(Scope::Session, vec![])),
        }));
        assert_eq!(error.code, code::NOT_FOUND);
        assert!(error.hint.contains("board"), "{}", error.hint);
        assert_eq!(error.see.as_deref(), Some("tilo-vm guide devices"));
    }
    // Phase `swaps_restart_the_devices_session_and_roll_back`.
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn_with(root.path(), "record-device");
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        let unit = "tilo-board-headless-session@quickuser.service";
        // Listed before anything was swapped into it: it is the machine's session.
        let fresh = client.call(Status { slot: None }).unwrap();
        assert_eq!(fresh.slots.len(), 1, "{:?}", fresh.slots);
        assert_eq!(
            (
                fresh.slots[0].name.as_str(),
                fresh.slots[0].kind,
                fresh.slots[0].generation
            ),
            ("device", SlotKind::Device, 0)
        );
        let v1 = import(client, "shell", &tarball("usr/bin/shell", b"v1"));
        let v2 = import(client, "shell", &tarball("usr/bin/shell", b"v2"));
        let swap = |layer: &str| Swap {
            slot: "device".into(),
            component: "shell".into(),
            layer: layer.into(),
            info: Some(ComponentInfo::new(Scope::Session, vec![])),
        };
        let applied = client.call(swap(&v1)).unwrap();
        assert_eq!(
            (applied.generation, applied.policy, applied.changed),
            (1, Policy::SessionRestart, true)
        );
        for unit in ["user@1000.service", unit, "tilo-board-drm-session.service"] {
            assert!(
                applied
                    .actions
                    .contains(&Action::WriteDropIn { unit: unit.into() }),
                "{unit}: {:?}",
                applied.actions
            );
        }
        let restart = [
            Action::StopUnit { unit: unit.into() },
            Action::RestartUnit {
                unit: "user@1000.service".into(),
            },
            Action::StartUnit { unit: unit.into() },
        ];
        assert!(
            applied.actions.windows(3).any(|w| w == restart),
            "{:?}",
            applied.actions
        );
        assert!(
            !applied
                .actions
                .iter()
                .any(|a| matches!(a, Action::EnsureUser { .. })),
            "the device's account is never a slot user: {:?}",
            applied.actions
        );
        assert_eq!(client.call(swap(&v2)).unwrap().generation, 2);

        let status = client
            .call(Status {
                slot: Some("device".into()),
            })
            .unwrap();
        let device = &status.slots[0];
        assert_eq!(device.kind, SlotKind::Device);
        assert_eq!(device.user.as_deref(), Some("quickuser"));
        assert_eq!(device.components["shell"], v2);
        assert_eq!(device.generations, [0, 1, 2]);
        assert!(status.issues.is_empty(), "{:?}", status.issues);
        let listed = client.call(SlotList {}).unwrap();
        assert_eq!(
            listed.iter().filter(|s| s.name == "device").count(),
            1,
            "{listed:?}"
        );

        let back = client
            .call(Rollback {
                slot: "device".into(),
                to: Some(0),
            })
            .unwrap();
        assert_eq!((back.generation, back.policy), (0, Policy::SessionRestart));
        assert!(
            back.actions
                .contains(&Action::StartUnit { unit: unit.into() })
        );
        let forward = client
            .call(Rollback {
                slot: "device".into(),
                to: Some(2),
            })
            .unwrap();
        assert_eq!(forward.generation, 2);

        let shown = client
            .call(SlotShow {
                name: "device".into(),
            })
            .unwrap();
        assert_eq!(shown.files.len(), 1, "{:?}", shown.files);
        assert_eq!(shown.files[0].layer, v2);
        let picture = client
            .call(Shot {
                slot: "device".into(),
            })
            .unwrap();
        assert!(!picture.png.is_empty());
    }
    // Phase `the_device_slot_is_neither_created_removed_nor_moved`.
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn_with(root.path(), "record-device");
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        let errors = [
            rpc_error(client.call(SlotCreate {
                name: "device".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })),
            rpc_error(client.call(SlotRm {
                name: "device".into(),
            })),
            rpc_error(client.call(SeatClaim {
                slot: "device".into(),
                owner: Owner::default(),
                steal: false,
            })),
            rpc_error(client.call(SeatRelease {
                slot: "device".into(),
            })),
        ];
        for error in errors {
            assert_eq!(error.code, code::INVALID, "{error:?}");
            assert!(
                error.hint.contains("rollback --slot device --to 0"),
                "{}",
                error.hint
            );
        }
        // Other slots on the same machine are ordinary slots.
        let created = client
            .call(SlotCreate {
                name: "agent-a".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })
            .unwrap();
        assert!(created.actions.contains(&Action::StartUnit {
            unit: "tilo-vm-session@agent-a.service".into()
        }));
    }
}

#[test]
fn chunked_upload_preserves_verified_layer_and_cache() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::spawn(root.path());
    daemon.client.hello("upload-ack-test").unwrap();
    // Send multiple chunks and leave a short final chunk.
    let data = vec![83_u8; tilo_vm_protocol::UPLOAD_CHUNK_BYTES * 5 + 17];
    let bytes = tarball("usr/bin/shell", &data);
    let hash = import(&mut daemon.client, "shell", &bytes);
    let installed =
        std::fs::read(root.path().join("layers").join(&hash).join("usr/bin/shell")).unwrap();
    assert_eq!(
        sha2::Sha256::digest(&installed),
        sha2::Sha256::digest(&data)
    );
    assert_eq!(installed.len(), data.len());
    let again = import(&mut daemon.client, "shell", &bytes);
    assert_eq!(again, hash, "chunked uploads preserve layer identity");
    let layers = daemon.client.call(LayerList {}).unwrap();
    assert_eq!(
        layers.len(),
        1,
        "the existing content cache reuses the layer"
    );
    assert_eq!(layers[0].hash, hash);
    assert_eq!(layers[0].provenance.component, "shell");

    let invalid = tarball("etc/unapproved", b"bad layer");
    let upload = daemon.client.upload(invalid.as_slice()).unwrap();
    assert!(
        daemon
            .client
            .call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(Provenance::local("shell")),
            })
            .is_err()
    );
    let retained = daemon.client.call(LayerList {}).unwrap();
    assert_eq!(retained.len(), 1, "rejected imports cannot publish a layer");
    assert_eq!(retained[0].hash, hash);
}

#[test]
fn verified_layer_cache_refuses_corruption_and_missing_entries() {
    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::spawn(root.path());
    daemon.client.hello("cache-fixture").unwrap();
    let bytes = tarball("usr/bin/shell", b"source-pinned payload");
    let hash = import(&mut daemon.client, "shell", &bytes);
    let verified = daemon
        .client
        .call(tilo_vm_protocol::LayerVerify { hash: hash.clone() })
        .unwrap();
    assert_eq!(verified.hash, hash);
    assert_eq!(verified.provenance.component, "shell");
    let missing = daemon.client.call(tilo_vm_protocol::LayerVerify {
        hash: "f".repeat(64),
    });
    assert!(missing.is_err());
    std::fs::write(
        root.path().join("layers").join(&hash).join("usr/bin/shell"),
        b"damaged cache",
    )
    .unwrap();
    assert!(
        daemon
            .client
            .call(tilo_vm_protocol::LayerVerify { hash: hash.clone() })
            .is_err()
    );
    // A normal reupload must not silently return the corrupt existing directory.
    let upload = daemon.client.upload(bytes.as_slice()).unwrap();
    assert!(
        daemon
            .client
            .call(LayerImport {
                source: LayerSource::Upload(upload),
                provenance: Box::new(Provenance::local("shell")),
            })
            .is_err()
    );
    std::fs::remove_dir_all(root.path().join("layers").join(&hash)).unwrap();
    let replaced = import(&mut daemon.client, "shell", &bytes);
    assert_eq!(replaced, hash);
    assert!(
        daemon
            .client
            .call(tilo_vm_protocol::LayerVerify { hash })
            .is_ok()
    );
}

#[test]
fn binary_upload_cancellation_cleans_partial_files_and_keeps_rpc_aligned() {
    struct Failing(std::io::Cursor<Vec<u8>>);
    impl std::io::Read for Failing {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.0.position() == self.0.get_ref().len() as u64 {
                return Err(std::io::Error::other("source cancelled"));
            }
            std::io::Read::read(&mut self.0, output)
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::spawn(dir.path());
    daemon.client.hello("binary-fixture").unwrap();
    assert!(matches!(
        daemon
            .client
            .upload(Failing(std::io::Cursor::new(vec![31; 2 << 20]))),
        Err(ClientError::Io(_))
    ));
    assert_eq!(
        std::fs::read_dir(dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
    assert!(daemon.client.call(LayerList {}).unwrap().is_empty());
    let bytes = tarball("usr/bin/binary-probe", &vec![57; (2 << 20) + 33]);
    let hash = import(&mut daemon.client, "binary-probe", &bytes);
    assert_eq!(daemon.client.call(LayerList {}).unwrap().len(), 1);
    assert_eq!(
        std::fs::read_dir(dir.path().join("uploads"))
            .unwrap()
            .count(),
        0
    );
    assert_eq!(
        std::fs::read(
            dir.path()
                .join("layers")
                .join(hash)
                .join("usr/bin/binary-probe")
        )
        .unwrap(),
        vec![57; (2 << 20) + 33]
    );
}

/// Runs one of tilo-vmd's boot-time commands against `root`, as its unit does.
fn boot_step(root: &std::path::Path, step: &str, booted: &str) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_tilo-vmd"))
        .args([step, "--apply", "record", "--manifest", "/nonexistent"])
        .args(["--base", "/nonexistent", "--booted", booted])
        .arg("--root")
        .arg(root)
        .output()
        .unwrap()
}

/// Layers are the person's deployed work. Booting another image keeps the
/// slot on its generation with its layer in place, and status names the
/// slot as running a layer built on the previous image; the person rolls it
/// back to the image deliberately and can roll forward again. A state root
/// that never recorded an image (2026-10-19: the first boot of a tilo-vmd
/// that records them dropped every slot's layers) is no change at all.
#[test]
fn booting_another_image_keeps_slots_and_layers_over_stdio() {
    let root = tempfile::tempdir().unwrap();
    let shell = {
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        client
            .call(SlotCreate {
                name: "agent-a".into(),
                kind: SlotKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
            })
            .unwrap();
        let shell = import(client, "shell", &tarball("usr/bin/shell", b"old shell"));
        let applied = client
            .call(Swap {
                slot: "agent-a".into(),
                component: "shell".into(),
                layer: shell.clone(),
                info: Some(ComponentInfo::new(Scope::Session, vec![])),
            })
            .unwrap();
        assert_eq!(applied.generation, 1);
        shell
    };

    // No image was ever recorded: the boot records this one, and the swap
    // stays.
    for step in ["reconcile-image", "restore"] {
        let first = boot_step(root.path(), step, "composefs:old");
        assert!(first.status.success(), "{first:?}");
    }
    {
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client.hello("test").unwrap();
        let status = client.call(Status { slot: None }).unwrap();
        assert_eq!(status.slots_image.as_deref(), Some("composefs:old"));
        assert_eq!(status.image_change, None);
        let slot = status.slots.iter().find(|s| s.name == "agent-a").unwrap();
        assert_eq!(slot.generation, 1);
    }

    // The upgrade's first boot.
    let upgraded = boot_step(root.path(), "reconcile-image", "composefs:new");
    assert!(upgraded.status.success(), "{upgraded:?}");
    let said = String::from_utf8_lossy(&upgraded.stderr);
    assert!(
        said.contains("slot agent-a stays on generation 1 (shell)"),
        "{said}"
    );
    let restored = boot_step(root.path(), "restore", "composefs:new");
    assert!(restored.status.success(), "{restored:?}");

    let mut daemon = Daemon::spawn(root.path());
    let client = &mut daemon.client;
    client.hello("test").unwrap();
    let status = client.call(Status { slot: None }).unwrap();
    assert_eq!(status.slots_image.as_deref(), Some("composefs:new"));
    let change = status.image_change.expect("the change is reported");
    assert_eq!(change.from.as_deref(), Some("composefs:old"));
    assert_eq!(change.to, "composefs:new");
    assert_eq!(change.kept.len(), 1);
    assert_eq!(change.kept[0].slot, "agent-a");
    assert_eq!(change.kept[0].components, vec!["shell".to_owned()]);
    assert!(change.dropped.is_empty());
    let slot = status.slots.iter().find(|s| s.name == "agent-a").unwrap();
    assert_eq!(slot.generation, 1, "the slot keeps its generation");
    assert!(
        client
            .call(LayerList {})
            .unwrap()
            .iter()
            .any(|layer| layer.hash == shell),
        "the layer is kept"
    );
    // Back to the image deliberately, and forward again.
    let back = client
        .call(Rollback {
            slot: "agent-a".into(),
            to: Some(0),
        })
        .unwrap();
    assert_eq!(back.generation, 0);
    let forward = client
        .call(Rollback {
            slot: "agent-a".into(),
            to: Some(1),
        })
        .unwrap();
    assert_eq!(forward.generation, 1);
}
