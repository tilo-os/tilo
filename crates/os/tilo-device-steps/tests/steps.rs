#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::path::Path;

use sha2::Digest as _;

use tilo_device_protocol::{
    AccountCreate, AccountKind, AccountList, AccountRm, AccountShow, Action, BaseInfo, Call,
    ComponentInfo, Gc, LayerImport, LayerList, LayerSource, Owner, Policy, Provenance, Rollback,
    RpcError, Scope, SeatClaim, SeatRelease, Shot, Status, Swap, code,
};
use tilo_device_steps::apply::RecordingApply;

/// A caller of the device's steps, as the daemon is: arguments staged in an invocation
/// directory, the step run on it, its `result.json` read back.
struct Steps {
    daemon: tilo_device_steps::Daemon,
    content: tempfile::TempDir,
    invocation: tempfile::TempDir,
}

impl Steps {
    #[allow(
        clippy::needless_pass_by_value,
        clippy::panic_in_result_fn,
        reason = "a call reads like the step it stands for, and a result file that is neither ok nor err fails the test"
    )]
    fn call<C: Call>(&mut self, args: C) -> Result<C::Output, RpcError> {
        let dir = self.invocation.path();
        let _ = std::fs::remove_file(dir.join("result.json"));
        std::fs::write(dir.join("args.json"), serde_json::to_vec(&args).unwrap()).unwrap();
        let exit = tilo_device_steps::run_step_on(&mut self.daemon, C::ACTION, dir);
        let mut result: serde_json::Map<String, serde_json::Value> =
            serde_json::from_slice(&std::fs::read(dir.join("result.json")).unwrap()).unwrap();
        let ok = result.remove("ok");
        let err = result.remove("err");
        assert_eq!(exit == std::process::ExitCode::SUCCESS, ok.is_some());
        assert!(ok.is_some() != err.is_some(), "a result is ok or err");
        ok.map(|ok| serde_json::from_value(ok).unwrap())
            .ok_or_else(|| serde_json::from_value(err.unwrap()).unwrap())
    }

    /// Put `bytes` into the content store, as the daemon's content puts do.
    fn upload(&self, bytes: &[u8]) -> String {
        let hash = blake3::hash(bytes).to_hex().to_string();
        std::fs::write(self.content.path().join(&hash), bytes).unwrap();
        hash
    }
}

struct Daemon {
    client: Steps,
}

impl Daemon {
    fn spawn(root: &Path) -> Self {
        Self::spawn_with(root, RecordingApply::default())
    }

    fn spawn_with(root: &Path, apply: RecordingApply) -> Self {
        let content = tempfile::tempdir().unwrap();
        let daemon =
            tilo_device_steps::Daemon::new(root, Box::new(apply), None, BaseInfo::default())
                .with_content(content.path());
        Self {
            client: Steps {
                daemon,
                content,
                invocation: tempfile::tempdir().unwrap(),
            },
        }
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

fn import(client: &mut Steps, component: &str, bytes: &[u8]) -> String {
    let upload = client.upload(bytes);
    let mut provenance = Provenance::local(component);
    provenance.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
    client
        .call(LayerImport {
            source: LayerSource::Content { hash: upload },
            provenance: Box::new(provenance),
        })
        .unwrap()
        .hash
}

fn rpc_error<T: std::fmt::Debug>(result: Result<T, RpcError>) -> RpcError {
    match result {
        Err(error) => error,
        other => panic!("expected a step error, got {other:?}"),
    }
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "each phase keeps one former test's body verbatim"
)]
fn account_and_kernel_lifecycles_through_steps() {
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;

        let status = client.call(Status { account: None }).unwrap();
        assert_eq!(status.arch, std::env::consts::ARCH);

        let created = client
            .call(AccountCreate {
                name: "agent-a".into(),
                kind: AccountKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
                device_identity: false,
            })
            .unwrap();
        assert!(created.created);
        assert!(created.actions.contains(&Action::StartUnit {
            unit: "tilo-vm-session@agent-a.service".into()
        }));
        let again = client
            .call(AccountCreate {
                name: "agent-a".into(),
                kind: AccountKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
                device_identity: false,
            })
            .unwrap();
        assert!(!again.created, "create is idempotent");

        let big = vec![7_u8; (1 << 20) * 2 + 17];
        let v1 = import(client, "shell", &tarball("usr/bin/shell", &big));
        assert_eq!(import(client, "shell", &tarball("usr/bin/shell", &big)), v1);
        let v2 = import(client, "shell", &tarball("usr/bin/shell", b"v2"));
        assert_eq!(client.call(LayerList {}).unwrap().len(), 2);

        let session = ComponentInfo::new(Scope::Session, vec![]);
        let swap = |layer: &str| Swap {
            account: "agent-a".into(),
            component: "shell".into(),
            layer: layer.into(),
            info: Some(session.clone()),
            partner: None,
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
                account: "agent-a".into(),
                to: None,
            })
            .unwrap();
        assert_eq!(back.generation, 1);
        let status = client
            .call(Status {
                account: Some("agent-a".into()),
            })
            .unwrap();
        assert_eq!(status.accounts[0].components["shell"], v1);
        // The base generation is listed too: rollback --to 0 reaches it.
        assert_eq!(status.accounts[0].generations, [0, 1, 2]);
        assert!(status.issues.is_empty(), "{:?}", status.issues);

        // account.show names each file the account sees, its digest and its layer.
        let shown = client
            .call(AccountShow {
                name: "agent-a".into(),
            })
            .unwrap();
        assert_eq!(shown.account.generation, 1);
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

        // VM-scope component into a user account: refused, with the right redirect.
        let fs_layer = import(client, "fs", &tarball("usr/bin/fs", b"fs"));
        let error = rpc_error(client.call(Swap {
            account: "agent-a".into(),
            component: "fs".into(),
            layer: fs_layer.clone(),
            info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
            partner: None,
        }));
        assert_eq!(error.code, code::SCOPE);
        assert!(
            error.hint.contains("tilo-vm swap --account vm fs"),
            "{}",
            error.hint
        );

        let vm = client
            .call(Swap {
                account: "vm".into(),
                component: "fs".into(),
                layer: fs_layer,
                info: Some(ComponentInfo::new(Scope::Vm, vec!["fs.service".into()])),
                partner: None,
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
            .call(AccountCreate {
                name: "agent-b".into(),
                kind: AccountKind::Headful,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
                device_identity: false,
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
                    account: "agent-a".into(),
                    owner,
                    steal: false
                })
                .unwrap()
                .changed
        );
        let error = rpc_error(client.call(SeatClaim {
            account: "agent-b".into(),
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
                account: "agent-b".into(),
                owner: Owner::default(),
                steal: true,
            })
            .unwrap();
        assert!(stolen.changed);
        let released = client
            .call(SeatRelease {
                account: "agent-b".into(),
            })
            .unwrap();
        assert_eq!(released.seat, "seat-agent-b");
        assert!(released.actions.contains(&Action::StartUnit {
            unit: "greetd.service".into()
        }));

        assert!(
            !client
                .call(Shot {
                    account: "agent-a".into()
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
                    account: Some("agent-a".into())
                })
                .unwrap()
                .accounts[0]
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
                .call(AccountRm {
                    name: "agent-a".into()
                })
                .unwrap()
                .removed
        );
        assert!(
            !client
                .call(AccountRm {
                    name: "agent-a".into()
                })
                .unwrap()
                .removed,
            "rm is idempotent"
        );
        assert_eq!(
            client.call(AccountList {}).unwrap().len(),
            2,
            "agent-b and vm remain"
        );
    }
    {
        use tilo_device_protocol::BootConfirm;

        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;

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
                account: "vm".into(),
                component: "kernel".into(),
                layer: layer.clone(),
                info: Some(kernel.clone()),
                partner: None,
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
                account: "vm".into(),
                component: "kernel".into(),
                layer,
                info: Some(kernel),
                partner: None,
            })
            .unwrap();
        assert!(!again.changed && again.boot.is_none());

        // Back to the base generation: the image's kernel is the next boot.
        let back = client
            .call(Rollback {
                account: "vm".into(),
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
fn steps_are_validated_and_routed_per_account() {
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let dir = daemon.client.invocation.path().to_owned();
        for (action, args) in [("account-list", &b"not json"[..]), ("no-such", b"{}")] {
            std::fs::write(dir.join("args.json"), args).unwrap();
            let exit = tilo_device_steps::run_step_on(&mut daemon.client.daemon, action, &dir);
            assert_eq!(exit, std::process::ExitCode::FAILURE);
            let result: serde_json::Value =
                serde_json::from_slice(&std::fs::read(dir.join("result.json")).unwrap()).unwrap();
            assert_eq!(result["err"]["code"], code::INVALID_PARAMS);
            assert!(!result["err"]["hint"].as_str().unwrap().is_empty());
        }
        let missing = rpc_error(daemon.client.call(LayerImport {
            source: LayerSource::Content {
                hash: "f".repeat(64),
            },
            provenance: Box::new(Provenance::local("shell")),
        }));
        assert_eq!(missing.code, code::NOT_FOUND);
        let not_a_hash = rpc_error(daemon.client.call(LayerImport {
            source: LayerSource::Content {
                hash: "../../etc/shadow".into(),
            },
            provenance: Box::new(Provenance::local("shell")),
        }));
        assert_eq!(not_a_hash.code, code::INVALID_PARAMS);
    }
    {
        use tilo_device_protocol::{AppInstall, Input, InputAction};
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        for (name, kind) in [("a", AccountKind::Headless), ("b", AccountKind::Headful)] {
            client
                .call(AccountCreate {
                    name: name.into(),
                    kind,
                    owner: Owner::default(),
                    outputs: vec![],
                    identity: false,
                    device_identity: false,
                })
                .unwrap();
        }
        let keys = vec![InputAction::Key {
            combos: vec!["ctrl+alt+t".into()],
        }];
        let error = rpc_error(client.call(Input {
            account: "a".into(),
            actions: keys.clone(),
        }));
        assert_eq!(error.code, code::INVALID);
        assert!(error.hint.contains("tilo-vm control"), "{}", error.hint);
        let sent = client
            .call(Input {
                account: "b".into(),
                actions: keys,
            })
            .unwrap();
        assert_eq!(sent.seat, "seat-b");
        assert!(sent.events > 0);
        let error = rpc_error(client.call(Input {
            account: "b".into(),
            actions: vec![InputAction::Key {
                combos: vec!["ctrl+nokey".into()],
            }],
        }));
        assert_eq!(error.code, code::INVALID_PARAMS);
        assert!(error.message.contains("nokey"), "{}", error.message);

        let upload = client.upload(&b"tapp bytes"[..]);
        let installed = client
            .call(AppInstall {
                account: "b".into(),
                source: LayerSource::Content { hash: upload },
            })
            .unwrap();
        assert_eq!(installed.account, "b");
        assert!(installed.id.is_some_and(|id| id.starts_with("so.tilo.")));
        let upload = client.upload(&b"tapp bytes"[..]);
        let error = rpc_error(client.call(AppInstall {
            account: "vm".into(),
            source: LayerSource::Content { hash: upload },
        }));
        assert!(error.hint.contains("headful or headless"), "{}", error.hint);
    }
}

tilo_test_env::scenario! {
    fn the_device_account_is_the_machines_session_on_its_physical_seat;

    phase device_login_input_uses_the_physical_seat;
    {
        use tilo_device_protocol::{Input, InputAction};

        let actions = vec![InputAction::Key {
            combos: vec!["a".into()],
        }];
        let root = tempfile::tempdir().unwrap();
        let mut login = Daemon::spawn_with(root.path(), RecordingApply::with_login("tester"));
        let sent = login
            .client
            .call(Input {
                account: "device".into(),
                actions: actions.clone(),
            })
            .unwrap();
        assert_eq!(sent.seat, "seat0");
        assert!(sent.events > 0);

        let board_root = tempfile::tempdir().unwrap();
        let mut board = Daemon::spawn_with(board_root.path(), RecordingApply::with_device("tester"));
        let error = rpc_error(board.client.call(Input {
            account: "device".into(),
            actions,
        }));
        assert_eq!(error.code, code::INVALID);
        assert!(error.message.contains("headless"));
    }

    phase the_device_account_is_the_machines_own_session;
    {
        {
            let root = tempfile::tempdir().unwrap();
            let mut daemon = Daemon::spawn(root.path());
            let client = &mut daemon.client;
            let layer = import(client, "shell", &tarball("usr/bin/shell", b"v1"));
            let error = rpc_error(client.call(Swap {
                account: "device".into(),
                component: "shell".into(),
                layer,
                info: Some(ComponentInfo::new(Scope::Session, vec![])),
                partner: None,
            }));
            assert_eq!(error.code, code::NOT_FOUND);
            assert!(error.hint.contains("board"), "{}", error.hint);
            assert_eq!(error.see.as_deref(), Some("tilo-vm guide devices"));
        }
        {
            let root = tempfile::tempdir().unwrap();
            let mut daemon = Daemon::spawn_with(root.path(), RecordingApply::with_device("tester"));
            let client = &mut daemon.client;
            let unit = "tilo-board-headless-session@tester.service";
            // Listed before anything was swapped into it: it is the machine's session.
            let fresh = client.call(Status { account: None }).unwrap();
            assert_eq!(fresh.accounts.len(), 1, "{:?}", fresh.accounts);
            assert_eq!(
                (
                    fresh.accounts[0].name.as_str(),
                    fresh.accounts[0].kind,
                    fresh.accounts[0].generation
                ),
                ("device", AccountKind::Device, 0)
            );
            let v1 = import(client, "shell", &tarball("usr/bin/shell", b"v1"));
            let v2 = import(client, "shell", &tarball("usr/bin/shell", b"v2"));
            let swap = |layer: &str| Swap {
                account: "device".into(),
                component: "shell".into(),
                layer: layer.into(),
                info: Some(ComponentInfo::new(Scope::Session, vec![])),
                partner: None,
            };
            let applied = client.call(swap(&v1)).unwrap();
            assert_eq!(
                (applied.generation, applied.policy, applied.changed),
                (1, Policy::SessionRestart, true)
            );
            // The seat's compositor is a system transient unit, which takes no
            // drop-in: the layers merge into the host `/usr` instead.
            assert!(
                applied.actions.contains(&Action::SysextRefresh),
                "{:?}",
                applied.actions
            );
            assert!(
                !applied
                    .actions
                    .iter()
                    .any(|a| matches!(a, Action::WriteDropIn { .. })),
                "{:?}",
                applied.actions
            );
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
                "the device's account is never an account user: {:?}",
                applied.actions
            );
            assert_eq!(client.call(swap(&v2)).unwrap().generation, 2);

            let status = client
                .call(Status {
                    account: Some("device".into()),
                })
                .unwrap();
            let device = &status.accounts[0];
            assert_eq!(device.kind, AccountKind::Device);
            assert_eq!(device.user.as_deref(), Some("tester"));
            assert_eq!(device.components["shell"], v2);
            assert_eq!(device.generations, [0, 1, 2]);
            assert!(status.issues.is_empty(), "{:?}", status.issues);
            let listed = client.call(AccountList {}).unwrap();
            assert_eq!(
                listed.iter().filter(|s| s.name == "device").count(),
                1,
                "{listed:?}"
            );

            let back = client
                .call(Rollback {
                    account: "device".into(),
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
                    account: "device".into(),
                    to: Some(2),
                })
                .unwrap();
            assert_eq!(forward.generation, 2);

            let shown = client
                .call(AccountShow {
                    name: "device".into(),
                })
                .unwrap();
            assert_eq!(shown.files.len(), 1, "{:?}", shown.files);
            assert_eq!(shown.files[0].layer, v2);
            let picture = client
                .call(Shot {
                    account: "device".into(),
                })
                .unwrap();
            assert!(!picture.png.is_empty());
        }
        {
            let root = tempfile::tempdir().unwrap();
            let mut daemon = Daemon::spawn_with(root.path(), RecordingApply::with_device("tester"));
            let client = &mut daemon.client;
            let errors = [
                rpc_error(client.call(AccountCreate {
                    name: "device".into(),
                    kind: AccountKind::Headless,
                    owner: Owner::default(),
                    outputs: vec![],
                    identity: false,
                    device_identity: false,
                })),
                rpc_error(client.call(AccountRm {
                    name: "device".into(),
                })),
                rpc_error(client.call(SeatClaim {
                    account: "device".into(),
                    owner: Owner::default(),
                    steal: false,
                })),
                rpc_error(client.call(SeatRelease {
                    account: "device".into(),
                })),
            ];
            for error in errors {
                assert_eq!(error.code, code::INVALID, "{error:?}");
                assert!(
                    error.hint.contains("rollback --account device --to 0"),
                    "{}",
                    error.hint
                );
            }
            let created = client
                .call(AccountCreate {
                    name: "agent-a".into(),
                    kind: AccountKind::Headless,
                    owner: Owner::default(),
                    outputs: vec![],
                    identity: false,
                    device_identity: false,
                })
                .unwrap();
            assert!(created.actions.contains(&Action::StartUnit {
                unit: "tilo-vm-session@agent-a.service".into()
            }));
        }
    }
}

tilo_test_env::scenario! {
    fn layer_content_is_verified_and_cached;

    phase large_content_preserves_verified_layer_and_cache;
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let data = vec![83_u8; (1 << 20) * 5 + 17];
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
        assert_eq!(again, hash, "importing the same content preserves layer identity");
        let layers = daemon.client.call(LayerList {}).unwrap();
        assert_eq!(
            layers.len(),
            1,
            "the existing content cache reuses the layer"
        );
        assert_eq!(layers[0].hash, hash);
        assert_eq!(layers[0].provenance.component, "shell");

        let invalid = tarball("etc/unapproved", b"bad layer");
        let upload = daemon.client.upload(invalid.as_slice());
        assert!(
            daemon
                .client
                .call(LayerImport {
                    source: LayerSource::Content { hash: upload },
                    provenance: Box::new(Provenance::local("shell")),
                })
                .is_err()
        );
        let retained = daemon.client.call(LayerList {}).unwrap();
        assert_eq!(retained.len(), 1, "rejected imports cannot publish a layer");
        assert_eq!(retained[0].hash, hash);
    }

    phase verified_layer_cache_refuses_corruption_and_missing_entries;
    {
        let root = tempfile::tempdir().unwrap();
        let mut daemon = Daemon::spawn(root.path());
        let bytes = tarball("usr/bin/shell", b"source-pinned payload");
        let hash = import(&mut daemon.client, "shell", &bytes);
        let verified = daemon
            .client
            .call(tilo_device_protocol::LayerVerify { hash: hash.clone() })
            .unwrap();
        assert_eq!(verified.hash, hash);
        assert_eq!(verified.provenance.component, "shell");
        let missing = daemon.client.call(tilo_device_protocol::LayerVerify {
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
                .call(tilo_device_protocol::LayerVerify { hash: hash.clone() })
                .is_err()
        );
        // A normal reupload must not silently return the corrupt existing directory.
        let upload = daemon.client.upload(bytes.as_slice());
        assert!(
            daemon
                .client
                .call(LayerImport {
                    source: LayerSource::Content { hash: upload },
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
                .call(tilo_device_protocol::LayerVerify { hash })
                .is_ok()
        );
    }
}

/// Runs the boot units' work against `root`, as `reconcile-image` and `restore` do, answering what
/// the image record says changed.
fn boot_step(root: &Path, step: &str, booted: &str) -> Vec<String> {
    let mut daemon = tilo_device_steps::Daemon::new(
        root,
        Box::new(RecordingApply::default()),
        None,
        BaseInfo::default(),
    );
    if step == "restore" {
        let issues = daemon.restore(Some(booted));
        assert!(issues.is_empty(), "{issues:?}");
        Vec::new()
    } else {
        daemon
            .reconcile_image(booted)
            .unwrap()
            .map(|change| change.describe())
            .unwrap_or_default()
    }
}

/// Layers are the person's deployed work: booting another image keeps the account on its generation
/// with its layer in place, and a layer that fits neither the new base nor its interfaces is held
/// rather than deleted and named in status. The person rolls the account back to the image
/// deliberately and can roll forward again, and a state root that never recorded an image is no
/// change at all.
#[test]
fn booting_another_image_keeps_accounts_and_layers() {
    let root = tempfile::tempdir().unwrap();
    let shell = {
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        client
            .call(AccountCreate {
                name: "agent-a".into(),
                kind: AccountKind::Headless,
                owner: Owner::default(),
                outputs: vec![],
                identity: false,
                device_identity: false,
            })
            .unwrap();
        let shell = import(client, "shell", &tarball("usr/bin/shell", b"old shell"));
        let applied = client
            .call(Swap {
                account: "agent-a".into(),
                component: "shell".into(),
                layer: shell.clone(),
                info: Some(ComponentInfo::new(Scope::Session, vec![])),
                partner: None,
            })
            .unwrap();
        assert_eq!(applied.generation, 1);
        shell
    };

    // No image was ever recorded: the boot records this one, and the swap
    // stays.
    for step in ["reconcile-image", "restore"] {
        boot_step(root.path(), step, "composefs:old");
    }
    {
        let mut daemon = Daemon::spawn(root.path());
        let client = &mut daemon.client;
        let status = client.call(Status { account: None }).unwrap();
        assert_eq!(status.accounts_image.as_deref(), Some("composefs:old"));
        assert_eq!(status.image_change, None);
        let account = status
            .accounts
            .iter()
            .find(|s| s.name == "agent-a")
            .unwrap();
        assert_eq!(account.generation, 1);
    }

    // The upgrade's first boot.
    let said = boot_step(root.path(), "reconcile-image", "composefs:new").join("\n");
    assert!(
        said.contains("account agent-a stays on generation 1 (shell)"),
        "{said}"
    );
    boot_step(root.path(), "restore", "composefs:new");

    let mut daemon = Daemon::spawn(root.path());
    let client = &mut daemon.client;
    let status = client.call(Status { account: None }).unwrap();
    assert_eq!(status.accounts_image.as_deref(), Some("composefs:new"));
    let change = status.image_change.expect("the change is reported");
    assert_eq!(change.from.as_deref(), Some("composefs:old"));
    assert_eq!(change.to, "composefs:new");
    assert_eq!(change.kept.len(), 1);
    assert_eq!(change.kept[0].account, "agent-a");
    assert_eq!(change.kept[0].components, vec!["shell".to_owned()]);
    assert!(change.dropped.is_empty());
    // Built here with neither a base digest nor interface versions: held,
    // kept but not merged, until a swap names it again.
    assert_eq!(change.held.len(), 1, "{:?}", change.held);
    assert_eq!(change.held[0].layer, shell);
    assert_eq!(change.held[0].component, "shell");
    let account = status
        .accounts
        .iter()
        .find(|s| s.name == "agent-a")
        .unwrap();
    assert_eq!(account.generation, 1, "the account keeps its generation");
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
            account: "agent-a".into(),
            to: Some(0),
        })
        .unwrap();
    assert_eq!(back.generation, 0);
    let forward = client
        .call(Rollback {
            account: "agent-a".into(),
            to: Some(1),
        })
        .unwrap();
    assert_eq!(forward.generation, 1);
}

/// A worktree arrives as content: the first sync lays it out, the next writes only what changed,
/// deletes what the manifest no longer names and keeps the builds' `target/`.
#[test]
fn tree_sync_lays_a_worktree_out_from_content() {
    use std::os::unix::fs::PermissionsExt as _;
    use tilo_device_protocol::{TreeFile, TreeManifest, TreeSymlink, TreeSync};

    let root = tempfile::tempdir().unwrap();
    let mut daemon = Daemon::spawn(root.path());
    let client = &mut daemon.client;
    let put = |client: &Steps, path: &str, mode: u32, bytes: &[u8]| TreeFile {
        path: path.into(),
        mode,
        hash: client.upload(bytes),
    };
    let first = TreeManifest {
        entries: vec![
            put(client, "Cargo.toml", 0o644, b"[workspace]"),
            put(client, "src/main.rs", 0o644, b"fn main() {}"),
            put(client, "tools/run.sh", 0o755, b"#!/bin/sh"),
            put(client, "old/gone.rs", 0o644, b"old"),
        ],
        symlinks: vec![TreeSymlink {
            path: "link".into(),
            target: "src/main.rs".into(),
        }],
    };
    let sync = |client: &mut Steps, manifest: &TreeManifest| {
        let hash = client.upload(&serde_json::to_vec(manifest).unwrap());
        client.call(TreeSync {
            worktree: "wt-1".into(),
            manifest: hash,
        })
    };
    let laid = sync(client, &first).unwrap();
    assert_eq!((laid.written, laid.removed), (5, 0));
    let tree = root.path().join("src/wt-1");
    assert_eq!(
        std::fs::read(tree.join("src/main.rs")).unwrap(),
        b"fn main() {}"
    );
    let mode = std::fs::metadata(tree.join("tools/run.sh"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o755);
    assert_eq!(
        std::fs::read_link(tree.join("link")).unwrap(),
        Path::new("src/main.rs")
    );
    std::fs::create_dir_all(tree.join("target/debug")).unwrap();
    std::fs::write(tree.join("target/debug/built"), b"x").unwrap();
    std::fs::write(tree.join("stray.txt"), b"x").unwrap();

    let mut second = first;
    second.entries.retain(|file| file.path != "old/gone.rs");
    second.entries[1] = put(client, "src/main.rs", 0o644, b"fn main() { run() }");
    let synced = sync(client, &second).unwrap();
    assert_eq!(synced.written, 1);
    assert_eq!(synced.removed, 2, "old/ and stray.txt go");
    assert!(!tree.join("old").exists());
    assert!(!tree.join("stray.txt").exists());
    assert!(tree.join("target/debug/built").is_file(), "target/ stays");
    assert_eq!(
        std::fs::read(tree.join("src/main.rs")).unwrap(),
        b"fn main() { run() }"
    );

    let mut escaping = second.clone();
    escaping.entries[0].path = "../outside".into();
    assert_eq!(
        sync(client, &escaping).unwrap_err().code,
        code::INVALID_PARAMS
    );
    let mut missing = second;
    missing.entries[0].hash = "0".repeat(64);
    assert_eq!(sync(client, &missing).unwrap_err().code, code::NOT_FOUND);
}
