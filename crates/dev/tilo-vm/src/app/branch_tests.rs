//! `vm branch` and `vm rm` of branches, from the parsed command line down to
//! the hypervisor commands they run (recorded by [`Hv`]) and the files and
//! registry they leave.

use std::collections::BTreeMap;

use clap::Parser as _;

use super::*;
use crate::cli::{BranchArgs, Cli};
use crate::config::Base;
use crate::hypervisor::Output;

const QCOW2: &[u8] = b"QFI\xfb\0\0\0\x03";
const SETUP: &[u8] = b"QFI\xfb dev as the person set it up";

/// The host's hypervisor tools as tilo-vm sees them: each command is
/// recorded; `virsh domstate`, `virsh list` and `utmctl status` answer from
/// `state` (default off); `qemu-img create` writes the disk it names; a
/// UTM clone gets an id of its own (unless `same_uuid`), which `osascript`
/// reports; a command containing `fail` fails.
#[derive(Default)]
struct Hv {
    ran: Vec<String>,
    state: BTreeMap<String, String>,
    fail: Option<String>,
    ids: BTreeMap<String, String>,
    same_uuid: bool,
}

impl Runner for Hv {
    fn output(&mut self, command: &CommandLine) -> Result<Output, CliError> {
        let line = command.display();
        self.ran.push(line.clone());
        if self.fail.as_deref().is_some_and(|fail| line.contains(fail)) {
            return Ok(Output {
                success: false,
                status: "exit status: 1".into(),
                stdout: String::new(),
                stderr: "refused".into(),
            });
        }
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        let stdout = match (command.program.as_str(), args.as_slice()) {
            ("qemu-img", [.., disk]) => {
                std::fs::write(disk, QCOW2).unwrap();
                String::new()
            }
            ("virsh", [_, _, "domstate", name]) => self
                .state
                .get(*name)
                .map_or("shut off", String::as_str)
                .to_owned(),
            ("virsh", [_, _, "list", ..]) => self
                .state
                .iter()
                .filter(|(_, state)| *state == "running")
                .flat_map(|(name, _)| [name.as_str(), "\n"])
                .collect(),
            ("utmctl", ["status", name]) => self
                .state
                .get(*name)
                .map_or("stopped", String::as_str)
                .to_owned(),
            ("utmctl", ["clone", from, "--name", name]) => {
                let id = if self.same_uuid {
                    self.ids[*from].clone()
                } else {
                    uuid::Uuid::new_v4().to_string().to_uppercase()
                };
                self.ids.insert((*name).to_owned(), id);
                String::new()
            }
            ("osascript", _) => args
                .iter()
                .find_map(|a| a.strip_prefix("return id of virtual machine \""))
                .and_then(|name| self.ids.get(name.trim_end_matches('"')))
                .cloned()
                .unwrap_or_default(),
            _ => String::new(),
        };
        Ok(Output {
            success: true,
            status: "exit status: 0".into(),
            stdout,
            stderr: String::new(),
        })
    }

    fn interactive(&mut self, command: &CommandLine) -> Result<bool, CliError> {
        self.output(command).map(|o| o.success)
    }
}

impl Hv {
    fn running(name: &str, state: &str) -> Self {
        Self {
            state: BTreeMap::from([(name.to_owned(), state.to_owned())]),
            ..Self::default()
        }
    }
}

/// The commands a run issued that name `path`: a VM's disk, or the domain
/// definition beside it. A frozen base is named after the VM it came from
/// with a random suffix (`bases/dev-1f3c9a….qcow2`), so its ending can spell
/// another disk's name: only the whole path tells the VM a run made from the
/// bases it read.
fn naming<'a>(ran: &'a [String], path: &str) -> Vec<&'a str> {
    ran.iter()
        .filter(|line| line.contains(path))
        .map(String::as_str)
        .collect()
}

fn branch_args(words: &str) -> BranchArgs {
    let line = format!("tilo-vm vm branch {words}");
    let Some(Command::Vm(VmCommand::Branch(args))) =
        Cli::try_parse_from(line.split(' ')).unwrap().command
    else {
        panic!("{line} is not vm branch");
    };
    args
}

struct Fixture {
    dir: tempfile::TempDir,
    app: App,
}

impl Fixture {
    /// One VM `dev` that `vm create` made with `hypervisor` and the person
    /// set up: its disk is a child of the published image.
    fn new(hypervisor: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let app = App {
            json: false,
            target: None,
            worktree: None,
            registry_path: dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir.path(), vec![]),
            warnings: Vec::new(),
            peer: None,
        };
        let image = dir.path().join("tilo-vm.qcow2");
        std::fs::write(&image, QCOW2).unwrap();
        let key = sshkey::key_path(&app.registry_path, "dev");
        std::fs::create_dir_all(key.parent().unwrap()).unwrap();
        std::fs::write(&key, "dev's private key").unwrap();
        std::fs::write(sshkey::public_path(&key), "ssh-ed25519 AAAA tilo-vm-dev\n").unwrap();
        let disk = (hypervisor != "utm").then(|| {
            let disk = dir.path().join("vms/dev.qcow2");
            std::fs::create_dir_all(disk.parent().unwrap()).unwrap();
            std::fs::write(&disk, SETUP).unwrap();
            disk.display().to_string()
        });
        let entry = VmEntry {
            hypervisor: hypervisor.into(),
            key: Some(key.display().to_string()),
            image: (hypervisor != "utm").then(|| image.display().to_string()),
            disk,
            ssh_port: matches!(hypervisor, "libvirt" | "whpx" | "qemu").then_some(2222),
            ssh: Some("root@127.0.0.1:2222".into()),
            template: (hypervisor == "utm").then(|| "Tilo Dev (dev)".into()),
            firmware_vars: (hypervisor == "whpx").then(|| "C:/qemu/vars.fd".into()),
            ..VmEntry::default()
        };
        let mut registry = Registry::default();
        registry.vms.insert("dev".into(), entry);
        registry.save(&app.registry_path).unwrap();
        Self { dir, app }
    }

    fn path(&self, rel: &str) -> String {
        self.dir.path().join(rel).display().to_string()
    }

    fn registry(&self) -> Registry {
        self.app.registry().unwrap()
    }

    fn branch(&self, hv: &mut Hv, words: &str) -> Result<Outcome, CliError> {
        self.app.branch_with(hv, &branch_args(words))
    }

    fn rm(&mut self, hv: &mut Hv, name: &str) -> Outcome {
        let Some(Command::Vm(VmCommand::Rm { name, force })) =
            Cli::try_parse_from(["tilo-vm", "vm", "rm", name])
                .unwrap()
                .command
        else {
            panic!("not vm rm");
        };
        self.app.vm_rm(hv, &name, force).unwrap()
    }

    /// The one base made from `from` whose users include `user`.
    fn base_of(&self, user: &str) -> (String, Base) {
        let registry = self.registry();
        let image = registry.vms[user].image.clone().unwrap();
        registry
            .bases
            .iter()
            .find(|(_, base)| base.disk == image)
            .map_or_else(
                || panic!("{user}'s image {image} is no base"),
                |(id, base)| (id.clone(), base.clone()),
            )
    }
}

/// A stopped VM's disk is frozen into a read-only base; the source gets a
/// fresh child at its old path and each branch another, with its own UUID
/// (domain and SMBIOS system UUID alike), MAC, SSH port and copy of the
/// root key. `vm list` shows the relationships; a rerun changes nothing.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the branch, then each property it must have"
)]
fn a_stopped_vm_branches_into_children_of_one_frozen_base() {
    let fx = Fixture::new("libvirt");
    let mut hv = Hv::default();
    let outcome = fx.branch(&mut hv, "dev a b").unwrap();
    let registry = fx.registry();
    assert_eq!(registry.bases.len(), 1, "{:?}", registry.bases);
    let (id, base) = fx.base_of("dev");
    assert!(id.starts_with("dev-"), "{id}");
    assert_eq!(base.from, "dev");
    assert_eq!(
        base.image.as_deref(),
        Some(fx.path("tilo-vm.qcow2").as_str())
    );
    assert_eq!(base.disk, fx.path(&format!("vms/bases/{id}.qcow2")));
    assert_eq!(
        std::fs::read(&base.disk).unwrap(),
        SETUP,
        "the base is dev's disk as it was"
    );
    assert!(
        std::fs::metadata(&base.disk)
            .unwrap()
            .permissions()
            .readonly()
    );

    let dev = &registry.vms["dev"];
    assert_eq!(dev.disk.as_deref(), Some(fx.path("vms/dev.qcow2").as_str()));
    assert_eq!(dev.image.as_deref(), Some(base.disk.as_str()));
    assert_eq!(dev.image_format, None);
    assert_eq!(
        std::fs::read(fx.path("vms/dev.qcow2")).unwrap(),
        QCOW2,
        "a fresh child"
    );
    assert_eq!(dev.uuid, None, "dev keeps the identity it had");
    let dev_key = std::fs::read(dev.key.as_deref().unwrap()).unwrap();

    let b = |name: &str| fx.path(&format!("vms/{name}.qcow2"));
    let virsh = "virsh --connect qemu:///session";
    let create = |disk: &str| format!("qemu-img create -f qcow2 -F qcow2 -b {} {disk}", base.disk);
    assert_eq!(
        hv.ran,
        [
            format!("LC_ALL=C {virsh} domstate dev"),
            create(&fx.path("vms/dev.qcow2")),
            create(&b("a")),
            format!("{virsh} define {}.xml", b("a")),
            create(&b("b")),
            format!("{virsh} define {}.xml", b("b")),
        ]
    );

    let mut identities = Vec::new();
    for name in ["a", "b"] {
        let entry = &registry.vms[name];
        assert_eq!(entry.image.as_deref(), Some(base.disk.as_str()), "{name}");
        assert_eq!(entry.disk.as_deref(), Some(b(name).as_str()));
        assert_eq!(entry.branched_from.as_deref(), Some("dev"));
        assert_eq!(entry.ssh, None, "{name} must not reach dev's address");
        let key = entry.key.as_deref().unwrap();
        assert_eq!(
            key,
            sshkey::key_path(&fx.app.registry_path, name)
                .display()
                .to_string()
        );
        assert_eq!(
            std::fs::read(key).unwrap(),
            dev_key,
            "the guest has dev's authorized_keys"
        );
        assert!(!Path::new(&remote::known_hosts_path(key)).exists());
        let uuid = entry.uuid.clone().unwrap();
        let mac = entry.nic_mac.clone().unwrap();
        assert!(uuid::Uuid::parse_str(&uuid).is_ok(), "{uuid}");
        assert!(mac.starts_with("52:54:00:") && mac.len() == 17, "{mac}");
        let xml = std::fs::read_to_string(format!("{}.xml", b(name))).unwrap();
        for needle in [
            format!("<name>{name}</name>"),
            format!("<uuid>{uuid}</uuid>"),
            format!("<entry name='uuid'>{uuid}</entry>"),
            format!("<mac address='{mac}'/>"),
            format!("<source file='{}'/>", b(name)),
            format!("<range start='{}' to='22'/>", entry.ssh_port.unwrap()),
        ] {
            assert!(
                xml.contains(&needle),
                "{name}'s domain lacks {needle}:\n{xml}"
            );
        }
        // Stable across boots: a later redefinition (swap kernel --direct)
        // keeps the recorded UUID without asking libvirt.
        let mut later = Hv::default();
        let vm = Vm { name, entry };
        hypervisor::Libvirt
            .redefine(&vm, "ssh-ed25519 AAAA", &mut later)
            .unwrap();
        assert!(
            later.ran.iter().all(|c| !c.contains("domuuid")),
            "{:?}",
            later.ran
        );
        let xml = std::fs::read_to_string(format!("{}.xml", b(name))).unwrap();
        assert!(
            xml.contains(&format!("<entry name='uuid'>{uuid}</entry>")),
            "{xml}"
        );
        identities.push((uuid, mac, entry.ssh_port));
    }
    assert_ne!(identities[0].0, identities[1].0, "UUIDs");
    assert_ne!(identities[0].1, identities[1].1, "MACs");
    assert_ne!(identities[0].2, identities[1].2, "SSH ports");
    assert!(identities.iter().all(|i| i.2 != Some(2222)), "dev's port");

    assert!(
        outcome.text.contains("new device identity"),
        "{}",
        outcome.text
    );
    assert_eq!(outcome.next[0], "tilo-vm vm start a");
    assert_eq!(outcome.data["base"]["users"], json!(["a", "b", "dev"]));
    let list = fx.app.vm_list().unwrap().text;
    assert!(list.contains("branch of dev"), "{list}");
    assert!(list.contains(&format!("{id}: dev's disk")), "{list}");
    assert!(list.contains("used by a, b, dev"), "{list}");

    let mut again = Hv::default();
    let rerun = fx.branch(&mut again, "dev a b").unwrap();
    assert!(rerun.text.contains("nothing to do"), "{}", rerun.text);
    assert!(again.ran.is_empty(), "{:?}", again.ran);
    assert_eq!(fx.registry(), registry);
}

/// What cannot be branched is refused before anything changes: a running,
/// paused or unknown-state source (with the --stop fix), names that clash,
/// a VM tilo-vm did not make, adapters without a branch path, and a disk
/// in the way.
#[test]
fn a_running_source_and_bad_requests_are_refused_without_a_change() {
    let fx = Fixture::new("libvirt");
    let before = fx.registry();
    let unchanged = |fx: &Fixture| {
        assert_eq!(fx.registry(), before);
        assert_eq!(std::fs::read(fx.path("vms/dev.qcow2")).unwrap(), SETUP);
        assert!(!Path::new(&fx.path("vms/bases")).exists());
        assert!(!Path::new(&fx.path("vms/a.qcow2")).exists());
    };
    for state in ["running", "paused", "pmsuspended"] {
        let error = fx
            .branch(&mut Hv::running("dev", state), "dev a")
            .err()
            .unwrap();
        assert!(matches!(error, CliError::Branch { .. }), "{error}");
        assert!(error.to_string().contains("running"), "{error}");
        assert!(
            error.hint().contains("tilo-vm vm branch dev a --stop"),
            "{}",
            error.hint()
        );
        assert_eq!(error.see().as_deref(), Some("tilo-vm guide concepts"));
        unchanged(&fx);
    }
    let mut silent = Hv {
        fail: Some("domstate".into()),
        ..Hv::default()
    };
    let error = fx.branch(&mut silent, "dev a").err().unwrap();
    assert!(
        error.to_string().contains("did not say whether it is off"),
        "{error}"
    );
    unchanged(&fx);

    for (words, says) in [
        ("dev dev", "twice"),
        ("dev a a", "twice"),
        ("dev ../a", "letters, digits"),
        ("dev .hidden", "letters, digits"),
    ] {
        let error = fx.branch(&mut Hv::default(), words).err().unwrap();
        assert!(error.to_string().contains(says), "{words}: {error}");
        unchanged(&fx);
    }
    assert!(matches!(
        fx.branch(&mut Hv::default(), "nope a"),
        Err(CliError::UnknownVm { .. })
    ));

    std::fs::write(fx.path("vms/a.qcow2"), "someone's").unwrap();
    let error = fx.branch(&mut Hv::default(), "dev a").err().unwrap();
    assert!(matches!(error, CliError::DiskExists { .. }), "{error}");
    std::fs::remove_file(fx.path("vms/a.qcow2")).unwrap();
    unchanged(&fx);

    let mut registry = fx.registry();
    let mut other = registry.vms["dev"].clone();
    other.branched_from = None;
    registry.vms.insert("taken".into(), other.clone());
    other.key = Some("/home/u/.ssh/id_ed25519".into());
    registry.vms.insert("shared".into(), other.clone());
    for adapter in ["qemu", "hyperv", "device"] {
        other.hypervisor = adapter.into();
        other.key = Some(
            sshkey::key_path(&fx.app.registry_path, adapter)
                .display()
                .to_string(),
        );
        registry.vms.insert(adapter.into(), other.clone());
    }
    registry.save(&fx.app.registry_path).unwrap();
    let error = fx.branch(&mut Hv::default(), "dev taken").err().unwrap();
    assert!(
        error.hint().contains("tilo-vm vm rm taken"),
        "{}",
        error.hint()
    );
    let error = fx.branch(&mut Hv::default(), "shared a").err().unwrap();
    assert!(error.to_string().contains("did not create it"), "{error}");
    for (adapter, says) in [
        ("qemu", "--hypervisor libvirt"),
        ("hyperv", "--hypervisor whpx"),
        ("device", "guide devices"),
    ] {
        let mut hv = Hv::default();
        let error = fx.branch(&mut hv, &format!("{adapter} a")).err().unwrap();
        assert!(error.hint().contains(says), "{adapter}: {}", error.hint());
        assert!(hv.ran.is_empty(), "{adapter}: {:?}", hv.ran);
    }
}

/// Removing the source keeps its branches bootable; the base lives until
/// the last VM using it goes, and then goes with it.
#[test]
fn removing_the_source_keeps_branches_and_the_base_until_the_last_user() {
    let mut fx = Fixture::new("libvirt");
    fx.branch(&mut Hv::default(), "dev a b").unwrap();
    let (id, base) = fx.base_of("a");

    let mut hv = Hv::default();
    let outcome = fx.rm(&mut hv, "dev");
    assert!(
        hv.ran
            .contains(&"virsh --connect qemu:///session undefine dev --nvram".to_owned()),
        "{:?}",
        hv.ran
    );
    assert!(!Path::new(&fx.path("vms/dev.qcow2")).exists());
    assert!(outcome.text.contains("kept its base"), "{}", outcome.text);
    assert!(outcome.text.contains("a, b still use"), "{}", outcome.text);
    assert_eq!(std::fs::read(&base.disk).unwrap(), SETUP);
    for name in ["a", "b"] {
        // What a boot of the branch reads: its own disk over the base.
        let entry = &fx.registry().vms[name];
        assert!(
            Path::new(entry.disk.as_deref().unwrap()).is_file(),
            "{name}"
        );
        assert_eq!(entry.image.as_deref(), Some(base.disk.as_str()));
        assert!(
            Path::new(entry.key.as_deref().unwrap()).is_file(),
            "{name}'s key"
        );
    }

    fx.rm(&mut Hv::default(), "a");
    assert!(Path::new(&base.disk).is_file(), "b still uses it");
    assert!(fx.registry().bases.contains_key(&id));

    let outcome = fx.rm(&mut Hv::default(), "b");
    assert!(!Path::new(&base.disk).exists(), "nothing uses it any more");
    assert!(fx.registry().bases.is_empty());
    assert!(
        outcome
            .text
            .contains(&format!("removed base {}", base.disk)),
        "{}",
        outcome.text
    );
    assert!(
        Path::new(&fx.path("tilo-vm.qcow2")).is_file(),
        "the image is never a base"
    );
}

/// A branch of a branch freezes the branch's disk into a base over the
/// first one; each base goes once nothing above it is left.
#[test]
fn a_branch_of_a_branch_chains_its_base_onto_the_first() {
    let mut fx = Fixture::new("libvirt");
    fx.branch(&mut Hv::default(), "dev a").unwrap();
    let (first, first_base) = fx.base_of("a");
    std::fs::write(fx.path("vms/a.qcow2"), b"QFI\xfb a's own changes").unwrap();
    fx.branch(&mut Hv::default(), "a a2").unwrap();
    let (second, second_base) = fx.base_of("a2");
    assert_ne!(first, second);
    assert_eq!(second_base.from, "a");
    assert_eq!(second_base.image.as_deref(), Some(first_base.disk.as_str()));
    assert_eq!(
        std::fs::read(&second_base.disk).unwrap(),
        b"QFI\xfb a's own changes"
    );
    assert_eq!(fx.base_of("a").0, second, "a moved onto the new base");
    assert_eq!(fx.registry().vms["a2"].branched_from.as_deref(), Some("a"));
    assert_eq!(
        fx.registry().base_users(&first_base.disk),
        ["dev", &format!("base {second}")]
    );

    fx.rm(&mut Hv::default(), "dev");
    fx.rm(&mut Hv::default(), "a");
    assert!(
        Path::new(&first_base.disk).is_file(),
        "a2's base is built on it"
    );
    assert!(Path::new(&second_base.disk).is_file());
    let outcome = fx.rm(&mut Hv::default(), "a2");
    assert!(!Path::new(&second_base.disk).exists());
    assert!(
        !Path::new(&first_base.disk).exists(),
        "the chain goes as a whole"
    );
    assert!(fx.registry().bases.is_empty(), "{}", outcome.text);
}

/// A failure midway keeps the branches made before it, registered and
/// working, removes what the failed one left, and the same command then
/// makes only the rest. A failure on the first branch puts the source's
/// disk back as it was.
#[test]
fn a_failure_midway_keeps_what_was_made_and_a_rerun_makes_the_rest() {
    let fx = Fixture::new("libvirt");
    let b = fx.path("vms/b.qcow2");
    let mut hv = Hv {
        fail: Some(format!("define {b}.xml")),
        ..Hv::default()
    };
    let error = fx.branch(&mut hv, "dev a b c").err().unwrap();
    assert!(matches!(error, CliError::Branch { .. }), "{error}");
    assert!(error.to_string().contains("making b failed"), "{error}");
    assert!(
        error.hint().contains("a was made and registered"),
        "{}",
        error.hint()
    );
    assert!(
        error.hint().contains("tilo-vm vm branch dev a b c"),
        "{}",
        error.hint()
    );
    let untouched = naming(&hv.ran, &fx.path("vms/c.qcow2"));
    assert!(untouched.is_empty(), "c is not attempted: {untouched:?}");
    assert!(
        hv.ran.iter().all(|c| !c.contains("undefine")),
        "b was never defined"
    );
    let registry = fx.registry();
    assert!(registry.vms.contains_key("a") && !registry.vms.contains_key("b"));
    for file in [b.clone(), format!("{b}.xml")] {
        assert!(!Path::new(&file).exists(), "{file} was left");
    }
    assert!(!sshkey::key_path(&fx.app.registry_path, "b").exists());
    let (first, base) = fx.base_of("a");
    assert_eq!(fx.base_of("dev").0, first);
    assert!(Path::new(&base.disk).is_file());

    let mut rerun = Hv::default();
    let outcome = fx.branch(&mut rerun, "dev a b c").unwrap();
    assert!(
        outcome.text.contains("already branched from dev: a"),
        "{}",
        outcome.text
    );
    let remade = naming(&rerun.ran, &fx.path("vms/a.qcow2"));
    assert!(remade.is_empty(), "a is already made: {remade:?}");
    let registry = fx.registry();
    assert!(
        ["a", "b", "c"]
            .iter()
            .all(|n| registry.vms.contains_key(*n))
    );
    assert_eq!(fx.base_of("a").0, first, "a keeps its base");
    assert_eq!(fx.base_of("b").1.image.as_deref(), Some(base.disk.as_str()));

    // The first branch fails: nothing changes.
    let fx = Fixture::new("libvirt");
    let before = fx.registry();
    let mut hv = Hv {
        fail: Some(fx.path("vms/a.qcow2")),
        ..Hv::default()
    };
    let error = fx.branch(&mut hv, "dev a b").err().unwrap();
    assert!(
        error.hint().contains("nothing was changed"),
        "{}",
        error.hint()
    );
    assert_eq!(fx.registry(), before);
    assert_eq!(std::fs::read(fx.path("vms/dev.qcow2")).unwrap(), SETUP);
    let permissions = std::fs::metadata(fx.path("vms/dev.qcow2"))
        .unwrap()
        .permissions();
    assert!(!permissions.readonly(), "dev's disk is writable again");
    let bases = std::fs::read_dir(fx.path("vms/bases")).unwrap().count();
    assert_eq!(bases, 0);
}

/// UTM: each branch is a clone of the stopped source (UTM clones the
/// bundle copy-on-write), which tilo-vm gives a new MAC and whose new UUID
/// it checks and records. A clone with the source's UUID is undone.
#[test]
fn utm_branches_are_clones_with_a_new_mac_and_a_checked_uuid() {
    let fx = Fixture::new("utm");
    let source_id = "0B8E5BFA-2D4C-4E8C-9C1D-2B7F1A3C9E10";
    let mut hv = Hv {
        ids: BTreeMap::from([("dev".to_owned(), source_id.to_owned())]),
        ..Hv::default()
    };
    fx.branch(&mut hv, "dev u1").unwrap();
    let registry = fx.registry();
    let u1 = &registry.vms["u1"];
    let mac = u1.nic_mac.clone().unwrap();
    assert_eq!(u1.template.as_deref(), Some("dev"));
    assert_eq!(
        u1.uuid.as_deref(),
        Some(hv.ids["u1"].to_lowercase().as_str())
    );
    assert_ne!(u1.uuid.as_deref(), Some(source_id.to_lowercase().as_str()));
    assert!(registry.bases.is_empty(), "APFS clones need no base");
    let script = |lines: &[&str]| {
        let mut command = CommandLine::new("osascript", &["-e", "tell application \"UTM\""]);
        for line in lines {
            command = command.arg("-e").arg(*line);
        }
        command.arg("-e").arg("end tell").display()
    };
    assert_eq!(
        hv.ran,
        [
            "utmctl status dev".to_owned(),
            script(&["return id of virtual machine \"dev\""]),
            "utmctl clone dev --name u1".to_owned(),
            script(&[
                "set m to virtual machine \"u1\"",
                "set c to configuration of m",
                "set n to network interfaces of c",
                "set i to item 1 of n",
                &format!("set address of i to \"{mac}\""),
                "set item 1 of n to i",
                "set network interfaces of c to n",
                "update configuration of m with c",
            ]),
            script(&["return id of virtual machine \"u1\""]),
        ]
    );

    let error = fx
        .branch(&mut Hv::running("dev", "started"), "dev u2")
        .err()
        .unwrap();
    assert!(error.hint().contains("--stop"), "{}", error.hint());

    let mut same = Hv {
        ids: BTreeMap::from([("dev".to_owned(), source_id.to_owned())]),
        same_uuid: true,
        ..Hv::default()
    };
    let error = fx.branch(&mut same, "dev u2").err().unwrap();
    assert!(error.to_string().contains("gave u2 dev's UUID"), "{error}");
    assert_eq!(
        same.ran.last().map(String::as_str),
        Some("utmctl delete u2")
    );
    assert!(!fx.registry().vms.contains_key("u2"));
    assert!(!sshkey::key_path(&fx.app.registry_path, "u2").exists());
}

/// WHPX: the base and children as on libvirt, and QEMU gets each VM's UUID
/// (`-uuid`, its SMBIOS system UUID) and MAC.
#[test]
fn whpx_branches_pass_their_uuid_and_mac_to_qemu() {
    let fx = Fixture::new("whpx");
    let mut hv = Hv::default();
    fx.branch(&mut hv, "dev w1").unwrap();
    let (_, base) = fx.base_of("w1");
    let w1 = fx.path("vms/w1.qcow2");
    assert!(
        hv.ran.contains(&format!(
            "qemu-img create -f qcow2 -F qcow2 -b {} {w1}",
            base.disk
        )),
        "{:?}",
        hv.ran
    );
    assert!(
        hv.ran
            .iter()
            .any(|c| c.contains("Copy-Item") && c.contains("w1.qcow2.vars.fd"))
    );
    let registry = fx.registry();
    let entry = &registry.vms["w1"];
    let start = hypervisor::Whpx.start(&Vm { name: "w1", entry })[0].display();
    let uuid = entry.uuid.as_deref().unwrap();
    let mac = entry.nic_mac.as_deref().unwrap();
    assert!(start.contains(&format!("\"-uuid\" \"{uuid}\"")), "{start}");
    assert!(
        start.contains(&format!("virtio-net-pci,netdev=net0,mac={mac}")),
        "{start}"
    );
}
