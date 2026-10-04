//! A device reached as its owner's account: `tilo-vm` registers it, then
//! creates a slot, swaps a layer into it, takes a picture and rolls back,
//! every remote command going through `sudo -n` to a real `tilo-vmd rpc`
//! (the side-effect-free `record` apply backend over a temporary root).
//!
//! The stand-in `ssh` runs the remote line in a local shell after it checks
//! the options tilo-vm passes; the stand-in `sudo` accepts only the
//! non-interactive form tilo-vm uses, and refuses like sudo without a grant
//! while a `deny` file exists.
#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::path::{Path, PathBuf};

use clap::Parser as _;
use tilo_vm::app::App;
use tilo_vm::cli::Cli;
use tilo_vm::error::CliError;
use tilo_vm::hypervisor::{SshEndpoint, SystemRunner};
use tilo_vm::receipt::Receipt;
use tilo_vm::remote::Remote;
use tilo_vm_protocol::{LayerImport, LayerSource, Provenance};

/// Write an executable script, closed before anything runs it.
fn script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt as _;
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

struct Device {
    dir: tempfile::TempDir,
    bin: PathBuf,
    log: PathBuf,
}

impl Device {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        let root = dir.path().join("state");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir_all(&root).unwrap();
        let log = dir.path().join("ssh.log");
        script(
            &bin.join("ssh"),
            &format!(
                "#!/bin/sh\n\
                 while [ $# -gt 0 ]; do\n\
                 \x20 case \"$1\" in\n\
                 \x20   -o|-i|-p) shift 2 ;;\n\
                 \x20   -t) shift ;;\n\
                 \x20   *@*) printf 'login %s\\n' \"$1\" >> {log}; shift; break ;;\n\
                 \x20   *) echo \"ssh stand-in: unexpected $1\" >&2; exit 255 ;;\n\
                 \x20 esac\n\
                 done\n\
                 printf '%s\\n' \"$*\" >> {log}\n\
                 PATH={bin}:$PATH exec sh -c \"$*\"\n",
                log = log.display(),
                bin = bin.display(),
            ),
        );
        script(
            &bin.join("sudo"),
            &format!(
                "#!/bin/sh\n\
                 if [ -e {deny} ] || [ \"$1\" != -n ] || [ \"$2\" != -- ]; then\n\
                 \x20 echo 'sudo: a password is required' >&2; exit 1\n\
                 fi\n\
                 shift 2\n\
                 exec \"$@\"\n",
                deny = bin.join("deny").display(),
            ),
        );
        script(
            &bin.join("tilo-vmd"),
            &format!(
                "#!/bin/sh\nshift\nexec {daemon} rpc --apply record --manifest /nonexistent --base /nonexistent --root {root} \"$@\"\n",
                daemon = env!("CARGO_BIN_EXE_tilo-vmd"),
                root = root.display(),
            ),
        );
        Self { dir, bin, log }
    }

    fn app(&self) -> App {
        App {
            json: false,
            worktree: None,
            registry_path: self.dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(self.dir.path(), vec![]),
            warnings: Vec::new(),
        }
    }

    fn run(&self, args: &[&str]) -> Result<tilo_vm::app::Outcome, CliError> {
        let cli =
            Cli::try_parse_from(std::iter::once("tilo-vm").chain(args.iter().copied())).unwrap();
        self.app().run(cli.normalized().command)
    }

    fn log(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap_or_default()
    }
}

#[test]
fn a_device_reached_as_its_owner_runs_everything_through_sudo() {
    let device = Device::new();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_VM_SSH", device.bin.join("ssh"))
        .remove("TILO_VM")
        .remove("TILO_VM_SLOT")
        .remove("TILO_VM_CONFIG");
    let endpoint = SshEndpoint::parse("me@device").unwrap();

    // Phase `without_passwordless_sudo_it_is_not_registered`.
    {
        std::fs::write(device.bin.join("deny"), "").unwrap();
        let refused = device
            .run(&["vm", "add", "yoga", "--ssh", "me@device"])
            .unwrap();
        assert!(refused.failed, "{}", refused.text);
        assert!(refused.text.contains("--grant-sudo"), "{}", refused.text);
        assert!(!device.dir.path().join("vms.toml").exists());
        let error = Remote::connect("yoga", &endpoint, None, None, Vec::new())
            .err()
            .unwrap();
        assert!(
            matches!(error, CliError::SudoRefused { ref user, .. } if user == "me"),
            "{error:?}"
        );
        assert!(
            error
                .hint()
                .contains("tilo-vm vm add yoga --ssh me@device --grant-sudo")
        );
        std::fs::remove_file(device.bin.join("deny")).unwrap();
    }

    // Phase `with_it_the_device_is_registered_and_checked`.
    {
        let added = device
            .run(&["vm", "add", "yoga", "--ssh", "me@device"])
            .unwrap();
        assert!(!added.failed, "{}", added.text);
        assert_eq!(added.data["registered"], serde_json::json!(true));
        let registry = std::fs::read_to_string(device.dir.path().join("vms.toml")).unwrap();
        assert!(registry.contains("hypervisor = \"device\""), "{registry}");
        assert!(registry.contains("ssh = \"me@device\""), "{registry}");
        assert!(registry.contains("default = \"yoga\""), "{registry}");
        // Rerunning changes nothing.
        let again = device
            .run(&["vm", "add", "yoga", "--ssh", "me@device"])
            .unwrap();
        assert!(again.text.contains("already registered"), "{}", again.text);
    }

    // Phase `slot_swap_shot_and_rollback_all_go_through_sudo`.
    {
        let created = device
            .run(&["slot", "create", "mine", "--headless"])
            .unwrap();
        assert!(!created.failed, "{}", created.text);
        // A layer the device already holds, as a build or CI pull leaves one.
        let layer = {
            let mut remote = Remote::connect("yoga", &endpoint, None, None, Vec::new()).unwrap();
            let mut builder = tar::Builder::new(Vec::new());
            let mut header = tar::Header::new_gnu();
            header.set_size(2);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, "usr/bin/cli-shim", &b"v1"[..])
                .unwrap();
            let upload = remote.upload(&builder.into_inner().unwrap()[..]).unwrap();
            let mut provenance = Provenance::local("cli-shim");
            provenance.target = Some(format!("{}-unknown-linux-gnu", std::env::consts::ARCH));
            remote
                .call(LayerImport {
                    source: LayerSource::Upload(upload),
                    provenance: Box::new(provenance),
                })
                .unwrap()
                .hash
        };
        let swapped = device
            .run(&[
                "swap",
                "cli-shim",
                "--slot",
                "mine",
                "--from",
                &format!("layer:{layer}"),
            ])
            .unwrap();
        assert_eq!(
            swapped.data["generation"],
            serde_json::json!(1),
            "{}",
            swapped.text
        );
        let png = device.dir.path().join("shot.png");
        let shot = device
            .run(&["shot", "--slot", "mine", "-o", png.to_str().unwrap()])
            .unwrap();
        assert!(!shot.failed, "{}", shot.text);
        assert!(std::fs::read(&png).unwrap().starts_with(b"\x89PNG"));
        let back = device.run(&["rollback", "--slot", "mine"]).unwrap();
        assert_eq!(
            back.data["generation"],
            serde_json::json!(0),
            "{}",
            back.text
        );
    }

    // Every login was the owner's, and every remote command but vm add's
    // probe (which checks sudo itself) ran under sudo -n.
    let log = device.log();
    let mut lines = log.lines();
    let mut commands = 0;
    while let Some(login) = lines.next() {
        assert_eq!(login, "login me@device", "{log}");
        let command = lines.next().unwrap();
        commands += 1;
        assert!(
            command.starts_with("sudo -n -- sh -c ") || command.starts_with("sh -c "),
            "{command}"
        );
        if command.starts_with("sh -c ") {
            assert!(
                command.contains("sudo -n true"),
                "only the probe runs unwrapped: {command}"
            );
        }
    }
    assert!(commands >= 8, "{log}");
}
