//! A device reached as its owner's account: `tilo-vm` registers it, then
//! creates a slot, swaps a layer into it, takes a picture and rolls back,
//! every remote command going through `sudo -n` to a real `tilo-vmd rpc`
//! (the side-effect-free `record` apply backend over a temporary root).
//!
//! The stand-in `ssh` runs the remote line in a local shell after it checks
//! the options tilo-vm passes; the stand-in `sudo` accepts only the
//! non-interactive forms tilo-vm uses (`sudo -n -- CMD`, and the probe's `sudo -n true`), and refuses like sudo without a grant
//! while a `deny` file exists.
#![allow(
    clippy::unwrap_used,
    clippy::too_many_lines,
    reason = "one end-to-end scenario; helpers fail the test on setup errors"
)]

use std::fmt::Write as _;
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
                 if [ -e {deny} ] || [ \"$1\" != -n ]; then\n\
                 \x20 echo 'sudo: a password is required' >&2; exit 1\n\
                 fi\n\
                 shift\n\
                 if [ \"$1\" = -- ]; then shift; fi\n\
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
            target: None,
            worktree: None,
            registry_path: self.dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(self.dir.path(), vec![]),
            warnings: Vec::new(),
            peer: None,
        }
    }

    fn run(&self, args: &[&str]) -> Result<tilo_vm::app::Outcome, CliError> {
        let cli =
            Cli::try_parse_from(std::iter::once("tilo-vm").chain(args.iter().copied())).unwrap();
        let cli = cli.normalized();
        let mut app = self.app();
        app.target = cli.target;
        app.run(cli.command)
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

/// Exercise the actual swap command twice against an isolated daemon. The
/// builder stands in for Cargo with one deterministic, source-bound layer;
/// no desktop, signing identity or application state is involved.
#[test]
fn repeated_remote_swap_reuses_verified_component_without_archive_transfer() {
    let device = Device::new();
    let source = device.dir.path().join("source");
    std::fs::create_dir_all(source.join("os")).unwrap();
    std::fs::write(
        source.join("os/components.toml"),
        "[[component]]\nname = \"cli-shim\"\npackages = [\"cli-shim\"]\nscope = \"session\"\n",
    )
    .unwrap();
    std::fs::write(source.join("Cargo.lock"), "fixture lock").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "os/components.toml", "Cargo.lock"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-qm",
            "source fixture",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .args(args)
                .current_dir(&source)
                .status()
                .unwrap()
                .success()
        );
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set("TILO_VM_SSH", device.bin.join("ssh"))
        .remove("TILO_VM")
        .remove("TILO_VM_SLOT")
        .remove("TILO_VM_CONFIG");
    assert!(
        !device
            .run(&["vm", "add", "fixture", "--ssh", "me@device"])
            .unwrap()
            .failed
    );
    assert!(
        !device
            .run(&["slot", "create", "mine", "--headless"])
            .unwrap()
            .failed
    );
    script(
        &device.bin.join("metadata"),
        "#!/bin/sh\nprintf '%s' '{\"packages\":[],\"target_directory\":\"target\"}'\n",
    );
    env.set("TILO_VM_CARGO", device.bin.join("metadata"));
    let builder = device.bin.join("fixture-builder.py");
    std::fs::write(&builder, r"
import base64,hashlib,io,json,sys,tarfile
args=sys.argv[1:]
inventory=args[args.index('--source-inventory')+1]
source=hashlib.sha256(open(inventory,'rb').read()).hexdigest()
payload=b'fixture payload'*8192
sha=hashlib.sha256(payload).hexdigest()
hash=hashlib.sha256(b'x\0usr/bin/cli-shim\0'+sha.encode()+b'\0').hexdigest()
cached=[args[i+1] for i,v in enumerate(args) if v=='--cached-layer']
reused=hash in cached
print('tilo-vm-layer-begin '+json.dumps({'component':'cli-shim','target':__import__('platform').machine()+'-unknown-linux-gnu','source_digest':source,'layer_hash':hash,'reused':reused}),flush=True)
data=b''
if not reused:
 stream=io.BytesIO()
 with tarfile.open(fileobj=stream,mode='w') as tar:
  directory=tarfile.TarInfo('usr');directory.type=tarfile.DIRTYPE;directory.mode=0o755;tar.addfile(directory)
  entry=tarfile.TarInfo('usr/bin/cli-shim');entry.mode=0o755;entry.size=len(payload);tar.addfile(entry,io.BytesIO(payload))
 data=stream.getvalue()
 print('tilo-vm-layer-data '+base64.b64encode(data).decode(),flush=True)
print('tilo-vm-layer-end '+hashlib.sha256(data).hexdigest()+' '+str(len(data)),flush=True)
").unwrap();
    let registry = device.dir.path().join("vms.toml");
    let mut config = std::fs::read_to_string(&registry).unwrap();
    write!(
        config,
        "\n[remote_builders]\n{} = [\"python3\", \"{}\"]\n",
        std::env::consts::ARCH,
        builder.display()
    )
    .unwrap();
    std::fs::write(&registry, config).unwrap();
    let mut receipts = Vec::new();
    for _ in 0..2 {
        let cli = Cli::try_parse_from([
            "tilo-vm",
            "swap",
            "cli-shim",
            "--slot",
            "mine",
            "--builder",
            "remote",
        ])
        .unwrap();
        let mut app = device.app();
        app.worktree = Some(tilo_vm::context::Worktree {
            root: source.clone(),
            branch: Some("fixture".into()),
        });
        let outcome = app.run(cli.command).unwrap();
        assert!(!outcome.failed, "{}", outcome.text);
        let path = app.receipt.finish(Ok(&outcome.data)).unwrap();
        receipts.push(
            serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap(),
        );
    }
    assert_ne!(receipts[0]["upload"], "skipped");
    assert_eq!(receipts[1]["upload"], "skipped");
    assert!(
        receipts[1]["layer_reused"]
            .as_str()
            .is_some_and(|hash| hash.len() == 64)
    );
    println!(
        "isolated repeated-swap receipts: {}",
        serde_json::to_string(&receipts).unwrap()
    );
}
