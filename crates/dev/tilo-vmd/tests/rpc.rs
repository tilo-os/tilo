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
        let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-vmd"))
            .args([
                "rpc",
                "--apply",
                "record",
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
        Self {
            child,
            client: Client::new(stdout, stdin),
        }
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
