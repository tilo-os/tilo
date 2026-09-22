//! Drives a real `tilo-devd rpc` subprocess through the production client
//! over stdio pipes, as `tilo-dev` does over SSH, against a temporary state
//! root with the side-effect-free `record` apply backend.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use tilo_dev_protocol::{
    Action, Client, ClientError, ComponentInfo, Gc, LayerImport, LayerList, LayerSource, Owner,
    PROTOCOL_VERSION, Policy, Provenance, Rollback, Scope, SeatClaim, SeatRelease, Shot,
    SlotCreate, SlotKind, SlotList, SlotRm, Status, Swap, code,
};

struct Daemon {
    child: Child,
    client: Client<BufReader<ChildStdout>, ChildStdin>,
}

impl Daemon {
    fn spawn(root: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-devd"))
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

fn rpc_error<T: std::fmt::Debug>(result: Result<T, ClientError>) -> tilo_dev_protocol::RpcError {
    match result {
        Err(ClientError::Rpc(error)) => error,
        other => panic!("expected an RPC error, got {other:?}"),
    }
}

#[test]
fn full_slot_lifecycle_over_stdio() {
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
        })
        .unwrap();
    assert!(created.created);
    assert!(created.actions.contains(&Action::StartUnit {
        unit: "tilo-dev-session@agent-a.service".into()
    }));
    let again = client
        .call(SlotCreate {
            name: "agent-a".into(),
            kind: SlotKind::Headless,
            owner: Owner::default(),
        })
        .unwrap();
    assert!(!again.created, "create is idempotent");

    // A multi-chunk upload lands as one content-addressed layer.
    let big = vec![7_u8; tilo_dev_protocol::UPLOAD_CHUNK_BYTES * 2 + 17];
    let v1 = import(client, "shell", &tarball("usr/bin/shell", &big));
    assert_eq!(import(client, "shell", &tarball("usr/bin/shell", &big)), v1);
    let v2 = import(client, "shell", &tarball("usr/bin/shell", b"v2"));
    assert_eq!(client.call(LayerList {}).unwrap().len(), 2);

    let session = ComponentInfo {
        scope: Scope::Session,
        units: vec![],
    };
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
    assert_eq!(status.slots[0].generations, [1, 2]);
    assert!(status.issues.is_empty(), "{:?}", status.issues);

    // VM-scope component into a user slot: refused, with the right redirect.
    let fs_layer = import(client, "fs", &tarball("usr/bin/fs", b"fs"));
    let error = rpc_error(client.call(Swap {
        slot: "agent-a".into(),
        component: "fs".into(),
        layer: fs_layer.clone(),
        info: Some(ComponentInfo {
            scope: Scope::Vm,
            units: vec!["fs.service".into()],
        }),
    }));
    assert_eq!(error.code, code::SCOPE);
    assert!(error.hint.contains("tilo-dev swap vm fs"), "{}", error.hint);

    // Into the vm slot it refreshes sysext, restarts its units and every slot session.
    let vm = client
        .call(Swap {
            slot: "vm".into(),
            component: "fs".into(),
            layer: fs_layer,
            info: Some(ComponentInfo {
                scope: Scope::Vm,
                units: vec!["fs.service".into()],
            }),
        })
        .unwrap();
    assert_eq!(vm.policy, Policy::VmRefresh);
    assert!(vm.actions.contains(&Action::SysextRefresh));
    assert!(vm.actions.contains(&Action::RestartUnit {
        unit: "fs.service".into()
    }));
    assert!(vm.actions.contains(&Action::StartUnit {
        unit: "tilo-dev-session@agent-a.service".into()
    }));

    // seat0 is a lease: a second claimant is told who holds it, unless it steals.
    client
        .call(SlotCreate {
            name: "agent-b".into(),
            kind: SlotKind::Headful,
            owner: Owner::default(),
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
        [1, 2]
    );
    let gc = client
        .call(Gc {
            keep: 0,
            dry_run: false,
        })
        .unwrap();
    assert_eq!(gc.removed_generations, ["agent-a/2"]);
    assert!(gc.removed_layers.is_empty());
    assert!(gc.storage.layers_bytes > 0);

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

#[test]
fn malformed_lines_get_error_responses() {
    use std::io::{BufRead, Write};
    // Raw lines, bypassing the typed client.
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_tilo-devd"))
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
