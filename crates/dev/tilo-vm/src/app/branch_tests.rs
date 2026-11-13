//! `vm branch` and `vm rm` of branches, from the parsed command line down to
//! the hypervisor commands they run (recorded by [`Hv`]) and the files and
//! registry they leave.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use clap::Parser as _;

use super::branch_admit::{Guest, Guests, Held};
use super::*;
use crate::cli::{BranchArgs, Cli};
use crate::config::Base;
use crate::hypervisor::Output;

const QCOW2: &[u8] = b"QFI\xfb\0\0\0\x03";
const SETUP: &[u8] = b"QFI\xfb dev as the person set it up";

#[derive(Default)]
struct Hv {
    ran: Vec<String>,
    log: Rc<RefCell<Vec<String>>>,
    state: BTreeMap<String, String>,
    fail: Option<String>,
    ids: BTreeMap<String, String>,
    same_uuid: bool,
}

impl Runner for Hv {
    fn output(&mut self, command: &CommandLine) -> Result<Output, CliError> {
        let line = command.display();
        self.ran.push(line.clone());
        self.log.borrow_mut().push(line.clone());
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
            ("virsh", [_, _, "start", name]) => {
                self.state.insert((*name).to_owned(), "running".into());
                String::new()
            }
            ("virsh", [_, _, "shutdown" | "destroy", name]) => {
                self.state.insert((*name).to_owned(), "shut off".into());
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
        self.app
            .branch_with(hv, &mut World::default(), &branch_args(words))
    }

    fn branch_in(&self, hv: &mut Hv, world: &mut World, words: &str) -> Result<Outcome, CliError> {
        self.app.branch_with(hv, world, &branch_args(words))
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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario: the branch, then each property it must have"
)]
fn a_stopped_vm_branches_into_children_of_one_frozen_base() {
    let fx = Fixture::new("libvirt");
    let mut hv = Hv::default();
    let outcome = fx.branch(&mut hv, "dev a b --no-admit").unwrap();
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
    let rerun = fx.branch(&mut again, "dev a b --no-admit").unwrap();
    assert!(rerun.text.contains("nothing to do"), "{}", rerun.text);
    assert!(again.ran.is_empty(), "{:?}", again.ran);
    assert_eq!(fx.registry(), registry);
}

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
    let error = fx.branch(&mut silent, "dev a --no-admit").err().unwrap();
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
        fx.branch(&mut Hv::default(), "nope a --no-admit"),
        Err(CliError::UnknownVm { .. })
    ));

    std::fs::write(fx.path("vms/a.qcow2"), "someone's").unwrap();
    let error = fx
        .branch(&mut Hv::default(), "dev a --no-admit")
        .err()
        .unwrap();
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
    let error = fx
        .branch(&mut Hv::default(), "dev taken --no-admit")
        .err()
        .unwrap();
    assert!(
        error.hint().contains("tilo-vm vm rm taken"),
        "{}",
        error.hint()
    );
    let error = fx
        .branch(&mut Hv::default(), "shared a --no-admit")
        .err()
        .unwrap();
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

#[test]
fn removing_the_source_keeps_branches_and_the_base_until_the_last_user() {
    let mut fx = Fixture::new("libvirt");
    fx.branch(&mut Hv::default(), "dev a b --no-admit").unwrap();
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

#[test]
fn a_branch_of_a_branch_chains_its_base_onto_the_first() {
    let mut fx = Fixture::new("libvirt");
    fx.branch(&mut Hv::default(), "dev a --no-admit").unwrap();
    let (first, first_base) = fx.base_of("a");
    std::fs::write(fx.path("vms/a.qcow2"), b"QFI\xfb a's own changes").unwrap();
    fx.branch(&mut Hv::default(), "a a2 --no-admit").unwrap();
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

#[test]
fn a_failure_midway_keeps_what_was_made_and_a_rerun_makes_the_rest() {
    let fx = Fixture::new("libvirt");
    let b = fx.path("vms/b.qcow2");
    let mut hv = Hv {
        fail: Some(format!("define {b}.xml")),
        ..Hv::default()
    };
    let error = fx.branch(&mut hv, "dev a b c --no-admit").err().unwrap();
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
    let outcome = fx.branch(&mut rerun, "dev a b c --no-admit").unwrap();
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

    let fx = Fixture::new("libvirt");
    let before = fx.registry();
    let mut hv = Hv {
        fail: Some(fx.path("vms/a.qcow2")),
        ..Hv::default()
    };
    let error = fx.branch(&mut hv, "dev a b --no-admit").err().unwrap();
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

#[test]
fn utm_branches_are_clones_with_a_new_mac_and_a_checked_uuid() {
    let fx = Fixture::new("utm");
    let source_id = "0B8E5BFA-2D4C-4E8C-9C1D-2B7F1A3C9E10";
    let mut hv = Hv {
        ids: BTreeMap::from([("dev".to_owned(), source_id.to_owned())]),
        ..Hv::default()
    };
    fx.branch(&mut hv, "dev u1 --no-admit").unwrap();
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
        .branch(&mut Hv::running("dev", "started"), "dev u2 --no-admit")
        .err()
        .unwrap();
    assert!(error.hint().contains("--stop"), "{}", error.hint());

    let mut same = Hv {
        ids: BTreeMap::from([("dev".to_owned(), source_id.to_owned())]),
        same_uuid: true,
        ..Hv::default()
    };
    let error = fx.branch(&mut same, "dev u2 --no-admit").err().unwrap();
    assert!(error.to_string().contains("gave u2 dev's UUID"), "{error}");
    assert_eq!(
        same.ran.last().map(String::as_str),
        Some("utmctl delete u2")
    );
    assert!(!fx.registry().vms.contains_key("u2"));
    assert!(!sshkey::key_path(&fx.app.registry_path, "u2").exists());
}

#[test]
fn whpx_branches_pass_their_uuid_and_mac_to_qemu() {
    let fx = Fixture::new("whpx");
    let mut hv = Hv::default();
    fx.branch(&mut hv, "dev w1 --no-admit").unwrap();
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

#[derive(Default)]
struct World {
    log: Rc<RefCell<Vec<String>>>,
    memory: Option<u64>,
    clock: Duration,
    answers: BTreeMap<String, VecDeque<Option<Guest>>>,
}

impl Guests for World {
    fn available_memory_mb(&mut self) -> Option<u64> {
        self.memory
    }

    fn read(
        &mut self,
        vm: &str,
        _slots: Option<&BTreeSet<String>>,
        _wake: bool,
    ) -> Result<Option<Guest>, CliError> {
        self.log.borrow_mut().push(format!("read {vm}"));
        let queue = self
            .answers
            .entry(vm.to_owned())
            .or_insert_with(|| VecDeque::from([Some(taken_in(vm))]));
        Ok(if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().cloned().unwrap()
        })
    }

    fn pause(&mut self) {
        self.clock = self.clock.saturating_add(Duration::from_secs(5));
    }

    fn elapsed(&self) -> Duration {
        self.clock
    }
}

fn taken_in(vm: &str) -> Guest {
    Guest {
        key: Some(format!("key of {vm}")),
        recorded: true,
        slots: BTreeMap::from([(
            "me".to_owned(),
            Some(vec![Held {
                room: "room-desk".into(),
                name: "Desk".into(),
                host: "dev".into(),
                hosted_here: vm == "dev",
                awaiting: false,
            }]),
        )]),
    }
}

fn coming_up(vm: &str) -> VecDeque<Option<Guest>> {
    let mut before = taken_in(vm);
    before.recorded = false;
    before.key = Some("key of dev".into());
    let mut waiting = taken_in(vm);
    if let Some(Some(held)) = waiting.slots.get_mut("me") {
        held[0].awaiting = true;
    }
    VecDeque::from([
        None,
        Some(before),
        Some(waiting.clone()),
        Some(waiting),
        Some(taken_in(vm)),
    ])
}

impl World {
    fn shared(log: &Rc<RefCell<Vec<String>>>) -> Self {
        Self {
            log: Rc::clone(log),
            ..Self::default()
        }
    }
}

fn steps(log: &Rc<RefCell<Vec<String>>>) -> Vec<String> {
    log.borrow()
        .iter()
        .filter_map(|line| {
            let words: Vec<&str> = line.split(' ').collect();
            match words.as_slice() {
                ["read", vm] => Some(format!("read {vm}")),
                ["virsh", _, _, verb @ ("start" | "shutdown"), vm] => Some(format!("{verb} {vm}")),
                _ => None,
            }
        })
        .collect()
}

fn shared_hv(log: &Rc<RefCell<Vec<String>>>) -> Hv {
    Hv {
        log: Rc::clone(log),
        ..Hv::default()
    }
}

#[test]
fn branches_boot_once_beside_their_source_to_be_taken_in_then_everything_stops() {
    let fx = Fixture::new("libvirt");
    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    world.answers.insert("a".into(), coming_up("a"));
    let outcome = fx.branch_in(&mut hv, &mut world, "dev a b").unwrap();
    assert_eq!(
        steps(&log),
        [
            "start dev",
            "read dev",
            "start a",
            "start b",
            "read a",
            "read b",
            "read a",
            "read a",
            "read a",
            "read a",
            "shutdown a",
            "shutdown b",
            "shutdown dev",
        ],
        "{:#?}",
        log.borrow()
    );
    assert!(
        outcome.text.contains("a, b booted once beside dev"),
        "{}",
        outcome.text
    );
    assert_eq!(outcome.data["admitted"], json!(["a", "b"]));
    let registry = fx.registry();
    for name in ["dev", "a", "b"] {
        assert!(registry.vms[name].awaits_host.is_none(), "{name}");
        assert_eq!(
            hv.state.get(name).map(String::as_str),
            Some("shut off"),
            "{name} is off again"
        );
    }
}

#[test]
fn branches_boot_in_batches_that_fit_the_hosts_memory() {
    let fx = Fixture::new("libvirt");
    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    world.memory = Some(branch_admit::HOST_RESERVE_MB + 3 * 8192);
    fx.branch_in(&mut hv, &mut world, "dev a b c").unwrap();
    assert_eq!(
        steps(&log),
        [
            "start dev",
            "read dev",
            "start a",
            "start b",
            "read a",
            "read b",
            "shutdown a",
            "shutdown b",
            "start c",
            "read c",
            "shutdown c",
            "shutdown dev",
        ]
    );
    assert_eq!(
        branch_admit::batches(("dev", 8192), &[("a".into(), 8192)], Some(4096)),
        Err("dev needs 8192 MiB to boot and the host has 4096 MiB free (keeping 2048 MiB for itself)".into())
    );

    let fx = Fixture::new("libvirt");
    let before = fx.registry();
    let mut world = World {
        memory: Some(6000),
        ..World::default()
    };
    let mut hv = Hv::default();
    let error = fx.branch_in(&mut hv, &mut world, "dev a").err().unwrap();
    assert!(
        error.to_string().contains("dev needs 8192 MiB to boot"),
        "{error}"
    );
    assert!(
        error.hint().contains("tilo-vm vm branch dev a --no-admit"),
        "{}",
        error.hint()
    );
    assert!(error.hint().contains("free memory"), "{}", error.hint());
    assert_eq!(fx.registry(), before, "nothing was made");
    assert!(
        hv.ran.iter().all(|c| !c.contains("qemu-img")),
        "{:?}",
        hv.ran
    );
}

#[test]
fn a_branch_not_taken_in_in_time_names_the_workspace_and_a_rerun_takes_it_in() {
    let fx = Fixture::new("libvirt");
    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    let mut waiting = taken_in("a");
    if let Some(Some(held)) = waiting.slots.get_mut("me") {
        held[0].awaiting = true;
    }
    world
        .answers
        .insert("a".into(), VecDeque::from([Some(waiting)]));
    let error = fx
        .branch_in(&mut hv, &mut world, "dev a --admit-timeout 20")
        .err()
        .unwrap();
    assert!(
        error
            .to_string()
            .contains("a: Desk (slot me) has not taken it in yet"),
        "{error}"
    );
    assert!(
        error.hint().contains("a made and registered, not booted"),
        "{}",
        error.hint()
    );
    assert!(
        error
            .hint()
            .contains("rerun to try again: tilo-vm vm branch dev a"),
        "{}",
        error.hint()
    );
    let last: Vec<String> = steps(&log).into_iter().rev().take(2).collect();
    assert_eq!(last, ["shutdown dev", "shutdown a"]);
    assert!(fx.registry().vms["a"].awaits_host.is_some());

    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    let outcome = fx.branch_in(&mut hv, &mut world, "dev a").unwrap();
    assert!(
        outcome.text.contains("a booted once beside dev"),
        "{}",
        outcome.text
    );
    assert!(
        hv.ran.iter().all(|c| !c.contains("qemu-img")),
        "nothing is made again"
    );
    assert_eq!(
        steps(&log),
        [
            "start dev",
            "read dev",
            "start a",
            "read a",
            "shutdown a",
            "shutdown dev"
        ]
    );
    assert!(fx.registry().vms["a"].awaits_host.is_none());

    let fx = Fixture::new("libvirt");
    let mut world = World::default();
    let mut kept = taken_in("b");
    kept.key = Some("key of dev".into());
    world
        .answers
        .insert("b".into(), VecDeque::from([Some(kept)]));
    let error = fx
        .branch_in(&mut Hv::default(), &mut world, "dev b")
        .err()
        .unwrap();
    assert!(
        error.to_string().contains("b kept dev's device key"),
        "{error}"
    );
    assert!(
        error.hint().contains("boot dev once on a current image"),
        "{}",
        error.hint()
    );
}

#[test]
fn no_admit_boots_nothing_and_a_running_source_stays_running() {
    let fx = Fixture::new("libvirt");
    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    let outcome = fx
        .branch_in(&mut hv, &mut world, "dev a --no-admit")
        .unwrap();
    assert!(steps(&log).is_empty(), "{:?}", steps(&log));
    assert!(
        outcome.text.contains("not booted (--no-admit)"),
        "{}",
        outcome.text
    );
    assert!(fx.registry().vms["a"].awaits_host.is_some());

    let log = Rc::default();
    let mut hv = shared_hv(&log);
    hv.state.insert("dev".into(), "running".into());
    let mut world = World::shared(&log);
    fx.branch_in(&mut hv, &mut world, "dev a").unwrap();
    assert_eq!(steps(&log), ["read dev", "start a", "read a", "shutdown a"]);
    assert_eq!(hv.state["dev"], "running");
    assert!(fx.registry().vms["a"].awaits_host.is_none());
}

#[test]
fn a_branch_that_does_not_boot_leaves_everything_made_and_off() {
    let fx = Fixture::new("libvirt");
    let log = Rc::default();
    let mut hv = Hv {
        fail: Some("start b".into()),
        ..shared_hv(&log)
    };
    let mut world = World::shared(&log);
    let error = fx.branch_in(&mut hv, &mut world, "dev a b").err().unwrap();
    assert!(error.to_string().contains("b did not boot"), "{error}");
    assert_eq!(
        steps(&log),
        [
            "start dev",
            "read dev",
            "start a",
            "start b",
            "shutdown a",
            "shutdown dev"
        ]
    );
    let registry = fx.registry();
    assert!(registry.vms["a"].awaits_host.is_some() && registry.vms["b"].awaits_host.is_some());
    assert!(
        hv.state.values().all(|state| state == "shut off"),
        "{:?}",
        hv.state
    );

    let log = Rc::default();
    let mut hv = shared_hv(&log);
    let mut world = World::shared(&log);
    fx.branch_in(&mut hv, &mut world, "dev a b").unwrap();
    let registry = fx.registry();
    assert!(registry.vms["a"].awaits_host.is_none() && registry.vms["b"].awaits_host.is_none());
}

#[test]
fn a_sessions_workspace_hosts_say_who_hosts_them_and_whether_they_wait() {
    let answer = json!({"answer": {"workspaces": [
        {"room": "room-desk", "name": "Desk", "host": "dev", "hosted_here": false, "awaiting_host": true},
        {"room": "room-old", "name": ""},
    ]}});
    assert_eq!(
        branch_admit::held_of(&answer),
        [
            Held {
                room: "room-desk".into(),
                name: "Desk".into(),
                host: "dev".into(),
                hosted_here: false,
                awaiting: true
            },
            Held {
                room: "room-old".into(),
                name: "room-old".into(),
                ..Held::default()
            },
        ]
    );
    let vm_stat = "Mach Virtual Memory Statistics: (page size of 16384 bytes)\nPages free:                               1000.\nPages active:                             5000.\nPages inactive:                           2000.\nPages speculative:                        1000.\n";
    assert_eq!(
        branch_admit::vm_stat_free_mb(vm_stat),
        Some((4000 * 16384) >> 20)
    );
}

#[test]
fn the_machine_record_is_read_by_one_shell_line_that_runs_the_whole_script() {
    let line = branch_admit::machine_line();
    assert_eq!(line.len(), 1, "{line:?}");
    let dir = tempfile::tempdir().unwrap();
    let private = dir.path().join("var/lib/tilo/device-private");
    std::fs::create_dir_all(&private).unwrap();
    std::fs::write(private.join("keypair.bin"), b"key").unwrap();
    let joined = line.join(" ").replace(
        "/var/lib/tilo/device-private",
        &private.display().to_string(),
    );
    let out = std::process::Command::new("sh")
        .args(["-c", &joined])
        .current_dir(dir.path())
        .output()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(
        said.contains("KEY="),
        "the whole script ran where the record is: {said:?}"
    );
}
