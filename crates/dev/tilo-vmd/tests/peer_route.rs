//! A device that is not on this network: `tilo-vm` finds it neither at its
//! address nor by name, dials it over the peer network through
//! `tilo-device-admin dial`, says which route it took, and drives a real
//! `tilo-vmd rpc` (the side-effect-free `record` backend) through it. A
//! refusal comes back as the device's reason with the fix.
//!
//! The stand-in dial helper checks the route tilo-vm recorded, prints the
//! preamble the real one prints, and execs the daemon; while a `deny` file
//! exists it refuses as a device does for a machine without the grant.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::path::Path;

use clap::Parser as _;
use tilo_vm::app::App;
use tilo_vm::cli::Cli;
use tilo_vm::config::{PeerRoute, Registry, VmEntry};
use tilo_vm::error::{CliError, PeerFailure};
use tilo_vm::hypervisor::SystemRunner;
use tilo_vm::receipt::Receipt;

fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_device_beyond_the_lan_is_driven_over_the_peer_network() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("state");
    std::fs::create_dir_all(&root).unwrap();
    let log = dir.path().join("dial.log");
    let deny = dir.path().join("deny");
    let helper = dir.path().join("tilo-device-admin");
    script(
        &helper,
        &format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$*\" >> {log}\n\
             case \" $* \" in *' dial '*' --endpoint ee11 --device dd22 --relay http://relay.test/ '*) ;; \
             *) echo \"tilo-device-admin: unexpected $*\" >&2; exit 1 ;; esac\n\
             if [ -e {deny} ]; then\n\
             \x20 echo 'tilo-device-admin: the device refused: NotAdministrator: enrol it for administration (tilo-vm vm enrol-admin)' >&2; exit 3\n\
             fi\n\
             printf '%s\\n' '{{\"tilo_device_admin\":{{\"device\":\"dd22\",\"label\":\"board\",\"route\":\"relay\",\"via\":\"http://relay.test/\",\"session\":4}}}}'\n\
             exec {daemon} rpc --apply record --manifest /nonexistent --base /nonexistent --root {root}\n",
            log = log.display(),
            deny = deny.display(),
            daemon = env!("CARGO_BIN_EXE_tilo-vmd"),
            root = root.display(),
        ),
    );
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_DEVICE_ADMIN", &helper)
        .remove("TILO_VM_SSH")
        .remove("TILO_VM")
        .remove("TILO_VM_SLOT")
        .remove("TILO_VM_CONFIG");
    let registry_path = dir.path().join("vms.toml");
    let mut registry = Registry {
        default: Some("board".into()),
        ..Registry::default()
    };
    registry.vms.insert(
        "board".into(),
        VmEntry {
            hypervisor: "device".into(),
            // TEST-NET-1: never answers, so the device is not on this network.
            ssh: Some("root@192.0.2.1:22".into()),
            peer: Some(PeerRoute {
                device: "dd22".into(),
                endpoint: "ee11".into(),
                identity: None,
                relay: Some("http://relay.test/".into()),
                home_relay: None,
            }),
            ..VmEntry::default()
        },
    );
    registry.save(&registry_path).unwrap();
    let run = |args: &[&str]| {
        let mut app = App {
            json: false,
            target: None,
            worktree: None,
            registry_path: registry_path.clone(),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir.path(), vec![]),
            warnings: Vec::new(),
            peer: None,
        };
        let cli =
            Cli::try_parse_from(std::iter::once("tilo-vm").chain(args.iter().copied())).unwrap();
        let cli = cli.normalized();
        app.target = cli.target;
        app.run(cli.command)
    };

    // Phase: status names the route, and slots work through it.
    let status = run(&["vm", "status", "board"])
        .map_err(|e| e.render())
        .unwrap();
    assert!(
        status
            .text
            .contains("reached over the peer network: relay http://relay.test/ (session 4"),
        "{}",
        status.text
    );
    assert_eq!(status.data["route"]["peer"]["device"], "dd22");
    let created = run(&["slot", "create", "peer-slot", "--headless"])
        .map_err(|e| e.render())
        .unwrap();
    assert!(created.text.contains("peer-slot"), "{}", created.text);
    let dials = std::fs::read_to_string(&log).unwrap();
    assert_eq!(dials.lines().count(), 2, "{dials}");
    assert!(
        dials.contains("--identity-root") && dials.contains("admin-identity"),
        "{dials}"
    );

    // Phase: a refusal is the device's reason, with the way to fix it.
    std::fs::write(&deny, "").unwrap();
    let refused = run(&["vm", "status", "board"]).unwrap();
    assert!(
        refused.text.contains("it refused this machine"),
        "{}",
        refused.text
    );
    let error = match run(&["slot", "list"]) {
        Err(error) => error,
        Ok(outcome) => panic!("listed slots on a device that refused: {}", outcome.text),
    };
    assert!(
        matches!(
            &error,
            CliError::PeerRoute {
                failure: PeerFailure::Refused,
                detail,
                ..
            } if detail.contains("NotAdministrator")
        ),
        "{error:?}"
    );
    assert!(
        error.hint().contains("tilo-vm vm enrol-admin"),
        "{}",
        error.hint()
    );
}
