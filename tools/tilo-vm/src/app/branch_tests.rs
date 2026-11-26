//! `vm branch` and `vm rm` of branches, from the parsed command line down to
//! the hypervisor commands they run (recorded by [`Hv`]) and the files and
//! registry they leave.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::rc::Rc;

use clap::Parser as _;

use super::branch_admit::{Guest, Guests, Held, Reach};
use super::*;
use crate::cli::{BranchArgs, Cli};
use crate::config::Base;
use crate::hypervisor::Output;

/// dev's NIC on the host network, as `vm create` gives a libvirt VM one.
const DEV_LAN: &str = "52:54:01:00:00:01";

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
            local: false,
            target: None,
            worktree: None,
            registry_path: dir.path().join("vms.toml"),
            runner: SystemRunner { cwd: None },
            receipt: Receipt::new(dir.path(), vec![]),
            warnings: Vec::new(),
            machine: None,
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
            lan: (hypervisor == "libvirt").then(|| DEV_LAN.into()),
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

tilo_test_env::scenario! {
    fn a_libvirt_vm_branches_onto_a_frozen_base;

    phase a_stopped_vm_branches_into_children_of_one_frozen_base;
    {
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
            assert!(!Path::new(&sshkey::known_hosts_path(key)).exists());
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

    phase a_running_source_and_bad_requests_are_refused_without_a_change;
    {
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

    phase removing_the_source_keeps_branches_and_the_base_until_the_last_user;
    {
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

    phase a_branch_of_a_branch_chains_its_base_onto_the_first;
    {
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

    phase a_failure_midway_keeps_what_was_made_and_a_rerun_makes_the_rest;
    {
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
}

#[derive(Default)]
struct World {
    log: Rc<RefCell<Vec<String>>>,
    memory: Option<u64>,
    clock: Duration,
    answers: BTreeMap<String, VecDeque<Option<Guest>>>,
    /// What a VM says when asked to reach an address on the host network: by default, that it
    /// did.
    reaches: BTreeMap<String, Reach>,
}

impl Guests for World {
    fn available_memory_mb(&mut self) -> Option<u64> {
        self.memory
    }

    fn read(
        &mut self,
        vm: &str,
        _accounts: Option<&BTreeSet<String>>,
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

    fn reach(&mut self, vm: &str, to: std::net::Ipv4Addr) -> Result<Option<Reach>, CliError> {
        self.log.borrow_mut().push(format!("reach {vm} {to}"));
        Ok(Some(self.reaches.get(vm).cloned().unwrap_or(Reach {
            address: None,
            reached: true,
        })))
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
        accounts: BTreeMap::from([(
            "me".to_owned(),
            Some(vec![Held {
                workspace: "workspace-shell".into(),
                name: "Shell".into(),
                host: "dev".into(),
                hosted_here: vm == "dev",
                awaiting: false,
            }]),
        )]),
    }
}

impl World {}

/// The repository file `path`, from this crate.
fn repo_file(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(path)
}

/// The reach check is one line SSH hands the guest's login shell, and it runs whole there: its
/// own address on tilolan0, and whether the source's answers.
#[cfg(unix)]
#[test]
fn the_reach_check_reads_the_branch_address_and_pings_the_target() {
    use std::os::unix::fs::PermissionsExt as _;
    use tilo_test_env::SpawnUnderEnvLock as _;
    let script = branch_admit::reach_script(std::net::Ipv4Addr::new(198, 18, 0, 1));
    let dir = tempfile::tempdir().unwrap();
    let asked = dir.path().join("ping-args");
    for (name, script) in [
        ("ip", "#!/bin/sh\necho \"3: tilolan0    inet 198.18.7.9/16 brd 198.18.255.255 scope global tilolan0\"\n".to_owned()),
        ("ping", format!("#!/bin/sh\necho \"$@\" > '{}'\n[ \"$5\" = 198.18.0.1 ]\n", asked.display())),
    ] {
        let path = dir.path().join(name);
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let out = std::process::Command::new("sh")
        .args(["-c", &script])
        .env("PATH", path)
        .output_locked()
        .unwrap();
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        branch_admit::read_reach(&said),
        Reach {
            address: Some("198.18.7.9/16".into()),
            reached: true
        },
        "{said:?}"
    );
    assert_eq!(
        std::fs::read_to_string(&asked).unwrap().trim(),
        "-c 3 -W 2 198.18.0.1"
    );
}

/// The dev image names the NIC whose MAC tilo-vm gives it `tilolan0`, leaves it to its own
/// unit rather than `NetworkManager`, and gives it the address tilo-vm computes from that MAC.
#[cfg(unix)]
#[test]
fn the_dev_image_gives_the_nic_the_address_tilo_vm_computes() {
    use tilo_test_env::SpawnUnderEnvLock as _;
    let script = repo_file("os/dev-image/usr/libexec/tilo/tilo-vm-lan");
    for mac in [
        "52:54:01:00:00:01",
        "52:54:01:00:0a:fe",
        "52:54:01:00:FF:7F",
    ] {
        let out = std::process::Command::new("sh")
            .arg(&script)
            .args(["address", mac])
            .output_locked()
            .unwrap();
        assert!(
            out.status.success(),
            "{mac}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            crate::lan::address(mac).unwrap().to_string(),
            "{mac}"
        );
    }
    let refused = std::process::Command::new("sh")
        .arg(&script)
        .args(["address", "52:54:00:00:01:02"])
        .output_locked()
        .unwrap();
    assert!(!refused.status.success());
    let rules = std::fs::read_to_string(repo_file(
        "os/dev-image/usr/lib/udev/rules.d/70-tilo-vm-lan.rules",
    ))
    .unwrap();
    assert!(
        rules.contains(&format!(
            "ATTR{{address}}==\"{}:*\", NAME=\"tilolan0\"",
            crate::lan::MAC_PREFIX
        )),
        "{rules}"
    );
    assert!(
        rules.contains("RUN+=\"/usr/bin/systemctl --no-block start tilo-vm-lan.service\""),
        "{rules}"
    );
    let unit = std::fs::read_to_string(repo_file(
        "os/dev-image/usr/lib/systemd/system/tilo-vm-lan.service",
    ))
    .unwrap();
    assert!(
        unit.contains("ExecStart=/usr/libexec/tilo/tilo-vm-lan"),
        "{unit}"
    );
    let nm = std::fs::read_to_string(repo_file(
        "os/dev-image/usr/lib/NetworkManager/conf.d/40-tilo-vm-lan.conf",
    ))
    .unwrap();
    assert!(
        nm.contains("match-device=interface-name:tilolan0") && nm.contains("managed=0"),
        "{nm}"
    );
}

/// What QEMU's socket multicast does with `localaddr=127.0.0.1`, done here as QEMU does it
/// (net/socket.c): each VM's socket binds the group and port, joins the group on the loopback
/// and sends there with loop on. A frame one sends reaches every other, on a host with no route
/// and no other interface, so the network needs nothing of the host's.
#[cfg(target_os = "linux")]
#[test]
fn a_frame_on_the_host_network_reaches_every_vm_over_loopback() {
    use std::net::{Ipv4Addr, UdpSocket};
    use std::os::fd::FromRawFd as _;
    let group: Ipv4Addr = crate::lan::GROUP.parse().unwrap();
    let port = 40_000_u16.saturating_add(u16::try_from(std::process::id() % 5_000).unwrap());
    let vm = || -> UdpSocket {
        // SAFETY: plain socket calls on a descriptor this function owns, with option values that
        // live across each call; the descriptor is handed to the UdpSocket that closes it.
        unsafe {
            let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
            assert!(fd >= 0);
            let one: libc::c_int = 1;
            let set = |level, name, value: *const libc::c_void, len| {
                assert_eq!(
                    libc::setsockopt(fd, level, name, value, len),
                    0,
                    "setsockopt {name}"
                );
            };
            let size = |n: usize| libc::socklen_t::try_from(n).unwrap();
            set(
                libc::SOL_SOCKET,
                libc::SO_REUSEADDR,
                (&raw const one).cast(),
                size(size_of::<libc::c_int>()),
            );
            let address = libc::sockaddr_in {
                sin_family: libc::sa_family_t::try_from(libc::AF_INET).unwrap(),
                sin_port: port.to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(group).to_be(),
                },
                sin_zero: [0; 8],
            };
            assert_eq!(
                libc::bind(
                    fd,
                    (&raw const address).cast(),
                    size(size_of::<libc::sockaddr_in>())
                ),
                0
            );
            let local = libc::in_addr {
                s_addr: u32::from(Ipv4Addr::LOCALHOST).to_be(),
            };
            let join = libc::ip_mreq {
                imr_multiaddr: libc::in_addr {
                    s_addr: u32::from(group).to_be(),
                },
                imr_interface: local,
            };
            set(
                libc::IPPROTO_IP,
                libc::IP_ADD_MEMBERSHIP,
                (&raw const join).cast(),
                size(size_of::<libc::ip_mreq>()),
            );
            set(
                libc::IPPROTO_IP,
                libc::IP_MULTICAST_LOOP,
                (&raw const one).cast(),
                size(size_of::<libc::c_int>()),
            );
            set(
                libc::IPPROTO_IP,
                libc::IP_MULTICAST_IF,
                (&raw const local).cast(),
                size(size_of::<libc::in_addr>()),
            );
            let socket = UdpSocket::from_raw_fd(fd);
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            socket
        }
    };
    let (a, b, c) = (vm(), vm(), vm());
    a.send_to(b"an ethernet frame", (group, port)).unwrap();
    for (name, socket) in [("b", &b), ("c", &c)] {
        let mut buf = [0_u8; 64];
        let (n, _) = socket
            .recv_from(&mut buf)
            .unwrap_or_else(|e| panic!("{name} heard nothing: {e}"));
        assert_eq!(&buf[..n], b"an ethernet frame", "{name}");
    }
}
