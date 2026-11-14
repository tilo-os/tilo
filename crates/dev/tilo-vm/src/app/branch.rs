//! `vm branch`: new VMs that start as copies of a stopped one.
//!
//! On libvirt and WHPX the source's disk is a read-only base with a
//! copy-on-write qcow2 child per VM, so no VM sees another's writes; on UTM each
//! branch is an APFS clone of the source's bundle. Every branch gets its own
//! name, UUID, MAC and SSH port.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::branch_admit::{self, Guests};
use super::{App, Outcome, remove_if_present};
use crate::cli::BranchArgs;
use crate::config::{Base, Registry, VmEntry};
use crate::error::CliError;
use crate::hypervisor::{self, Branching, Hypervisor, Runner, SystemRunner, Vm, run_all};
use crate::{forwarded_port, remote, sshkey};

/// How long `--stop` waits for the hypervisor to report the source off
/// after its guest shut down.
const STOP_WAIT: Duration = Duration::from_secs(60);

struct Frozen {
    id: String,
    base: String,
    disk: String,
    image: Option<String>,
    image_format: Option<String>,
    permissions: std::fs::Permissions,
}

struct Child {
    name: String,
    entry: VmEntry,
    commands: Vec<String>,
}

impl App {
    pub(super) fn vm_branch(&mut self, args: &BranchArgs) -> Result<Outcome, CliError> {
        let mut runner = SystemRunner {
            cwd: self.runner.cwd.clone(),
        };
        if args.stop {
            self.stop_for_branch(&mut runner, &args.source)?;
        }
        let mut guests = branch_admit::LiveGuests {
            app: self.sibling(),
            started: Instant::now(),
            woken: std::collections::BTreeSet::new(),
        };
        self.branch_with(&mut runner, &mut guests, args)
    }

    fn sibling(&self) -> App {
        App {
            json: self.json,
            target: None,
            worktree: None,
            registry_path: self.registry_path.clone(),
            runner: SystemRunner {
                cwd: self.runner.cwd.clone(),
            },
            receipt: crate::receipt::Receipt::new(&self.base_dir(), Vec::new()),
            warnings: Vec::new(),
            peer: None,
        }
    }

    fn stop_for_branch(&mut self, runner: &mut dyn Runner, source: &str) -> Result<(), CliError> {
        let vm = self.resolve_vm(Some(source))?;
        let adapter = Self::adapter(&vm)?;
        if adapter.stopped(&vm.vm(), runner) == Some(true) {
            return Ok(());
        }
        let stopped = self.vm_stop(Some(source), false)?;
        if !self.json {
            eprintln!("tilo-vm: {}", stopped.text);
        }
        let started = Instant::now();
        while adapter.stopped(&vm.vm(), runner) == Some(false) && started.elapsed() < STOP_WAIT {
            std::thread::sleep(Duration::from_millis(500));
        }
        Ok(())
    }

    pub(super) fn branch_with(
        &self,
        runner: &mut dyn Runner,
        guests: &mut dyn Guests,
        args: &BranchArgs,
    ) -> Result<Outcome, CliError> {
        let mut registry = self.registry()?;
        let src = args.source.as_str();
        let rerun = format!("tilo-vm vm branch {src} {}", args.names.join(" "));
        let source = registry
            .vms
            .get(src)
            .cloned()
            .ok_or_else(|| CliError::UnknownVm {
                name: src.to_owned(),
                registry: self.registry_path.display().to_string(),
                known: registry.vms.keys().cloned().collect(),
            })?;
        let adapter = self.branchable(src, &source)?;
        let (todo, skipped) = Self::new_names(&registry, src, &args.names)?;
        let unadmitted: Vec<String> = skipped
            .iter()
            .filter(|name| {
                registry
                    .vms
                    .get(*name)
                    .is_some_and(|e| e.awaits_host.as_deref() == Some(src))
            })
            .cloned()
            .collect();
        let timeout = Duration::from_secs(args.admit_timeout);
        if todo.is_empty() && !args.no_admit && !unadmitted.is_empty() {
            let admitted = self.admit(runner, guests, (src, &rerun), &unadmitted, timeout)?;
            return Ok(Outcome::new(
                format!(
                    "{} booted once beside {src} and taken into the workspaces {src} hosts; stopped again",
                    admitted.join(", ")
                ),
                json!({ "source": src, "created": [], "skipped": skipped, "admitted": admitted }),
            )
            .next(&["tilo-vm vm list"]));
        }
        if todo.is_empty() {
            return Ok(Outcome::new(
                format!(
                    "{} already branched from {src}; nothing to do",
                    skipped.join(", ")
                ),
                json!({ "source": src, "created": [], "skipped": skipped }),
            )
            .next(&["tilo-vm vm list"]));
        }
        Self::ensure_stopped(runner, adapter.as_ref(), (src, &source), &rerun)?;
        if !args.no_admit {
            Self::admission_fits(guests, &registry, src, &rerun)?;
        }
        let root_key = sshkey::read_public(&sshkey::key_path(&self.registry_path, src))?;
        let (frozen, source_uuid) = self.prepare(
            runner,
            &mut registry,
            adapter.as_ref(),
            (src, &rerun),
            &todo,
        )?;
        let created = self.make_all(
            runner,
            &mut registry,
            adapter.as_ref(),
            (src, &rerun),
            &todo,
            (&root_key, source_uuid.as_deref()),
            frozen.as_ref(),
        )?;
        let mut outcome = Self::branched(src, &registry, frozen.as_ref(), &created, &skipped);
        if args.no_admit {
            let _ = write!(
                outcome.text,
                "\n  not booted (--no-admit): each is taken into the workspaces {src} hosts at its first boot while {src} runs"
            );
            return Ok(outcome);
        }
        let mut names: Vec<String> = created.iter().map(|c| c.name.clone()).collect();
        names.extend(unadmitted);
        self.admit_made(runner, guests, (src, &rerun), &names, timeout, outcome)
    }

    fn admit_made(
        &self,
        runner: &mut dyn Runner,
        guests: &mut dyn Guests,
        (src, rerun): (&str, &str),
        names: &[String],
        timeout: Duration,
        mut outcome: Outcome,
    ) -> Result<Outcome, CliError> {
        let admitted = self
            .admit(runner, guests, (src, rerun), names, timeout)
            .map_err(|error| match error {
                CliError::Branch { what, fix } => CliError::Branch {
                    what,
                    fix: format!(
                        "{} made and registered, not booted; {fix}",
                        names.join(", ")
                    ),
                },
                other => other,
            })?;
        let _ = write!(
            outcome.text,
            "\n  {} booted once beside {src}, re-keyed and taken into the workspaces {src} hosts, then stopped again",
            admitted.join(", ")
        );
        if let Some(data) = outcome.data.as_object_mut() {
            data.insert("admitted".into(), json!(admitted));
        }
        Ok(outcome)
    }

    /// Refuse a source that runs, or one whose hypervisor cannot say: a running VM writes its disk.
    fn ensure_stopped(
        runner: &mut dyn Runner,
        adapter: &dyn Hypervisor,
        (src, source): (&str, &VmEntry),
        rerun: &str,
    ) -> Result<(), CliError> {
        match adapter.stopped(
            &Vm {
                name: src,
                entry: source,
            },
            runner,
        ) {
            Some(true) => {}
            Some(false) => {
                return Err(refuse(
                    format!("{src}: it is running, and a running VM writes its disk"),
                    format!(
                        "let tilo-vm shut it down cleanly first: {rerun} --stop   |   or tilo-vm vm stop {src}, then rerun"
                    ),
                ));
            }
            None => {
                return Err(refuse(
                    format!("{src}: {} did not say whether it is off", adapter.name()),
                    format!(
                        "check that the hypervisor's tools answer (tilo-vm doctor), shut {src} down (tilo-vm vm stop {src}) and rerun"
                    ),
                ));
            }
        }
        Ok(())
    }

    fn make_all(
        &self,
        runner: &mut dyn Runner,
        registry: &mut Registry,
        adapter: &dyn Hypervisor,
        (src, rerun): (&str, &str),
        todo: &[String],
        (root_key, source_uuid): (&str, Option<&str>),
        frozen: Option<&Frozen>,
    ) -> Result<Vec<Child>, CliError> {
        let mut created: Vec<Child> = Vec::new();
        for name in todo {
            let made = self.branch_one(
                runner,
                registry,
                adapter,
                (src, name),
                root_key,
                source_uuid,
            );
            match made {
                Ok(child) => created.push(child),
                Err(error) => {
                    let mut note = String::new();
                    if let (true, Some(frozen)) = (created.is_empty(), frozen) {
                        // Nothing was branched: give the source its disk back.
                        if let Err(undo) = self.unfreeze(registry, src, frozen) {
                            note =
                                format!(" ({src} keeps its new disk on the frozen base: {undo})");
                        }
                    }
                    let names: Vec<String> = created.iter().map(|c| c.name.clone()).collect();
                    let mut error = failed(src, &format!("making {name}"), &error, rerun, &names);
                    if let CliError::Branch { fix, .. } = &mut error {
                        fix.push_str(&note);
                    }
                    return Err(error);
                }
            }
        }
        Ok(created)
    }

    fn branchable(&self, src: &str, source: &VmEntry) -> Result<Box<dyn Hypervisor>, CliError> {
        let adapter = hypervisor::adapter(&source.hypervisor)?;
        let kind = adapter.branching();
        if let Branching::Unsupported(alternative) = kind {
            return Err(refuse(
                format!("{src}: it runs on {}", adapter.name()),
                alternative,
            ));
        }
        let own_key = sshkey::key_path(&self.registry_path, src);
        if source.key.as_deref().map(Path::new) != Some(own_key.as_path()) {
            return Err(refuse(
                format!("{src}: tilo-vm did not create it, so its disk is not tilo-vm's to freeze"),
                "branch a VM tilo-vm made: tilo-vm vm create <name>, set it up, then tilo-vm vm branch <name> <new>".into(),
            ));
        }
        Ok(adapter)
    }

    fn prepare(
        &self,
        runner: &mut dyn Runner,
        registry: &mut Registry,
        adapter: &dyn Hypervisor,
        (src, rerun): (&str, &str),
        todo: &[String],
    ) -> Result<(Option<Frozen>, Option<String>), CliError> {
        let source = registry.vms.get(src).cloned().unwrap_or_default();
        let kind = adapter.branching();
        let frozen = if kind == Branching::Backing {
            let disk = source.disk.clone().ok_or_else(|| {
                refuse(
                    format!("{src}: it has no `disk`"),
                    format!(
                        "tilo-vm vm list shows {src}'s entry; recreate it with tilo-vm vm create"
                    ),
                )
            })?;
            if !Path::new(&disk).is_file() {
                return Err(refuse(
                    format!("{src}: its disk {disk} is missing"),
                    format!("tilo-vm vm rm {src} unregisters it; then tilo-vm vm create a new one"),
                ));
            }
            for name in todo {
                let path = child_disk(&disk, name);
                if Path::new(&path).exists() {
                    return Err(CliError::DiskExists { path });
                }
            }
            Some(
                self.freeze(runner, registry, src, &disk)
                    .map_err(|error| failed(src, "freezing its disk", &error, rerun, &[]))?,
            )
        } else {
            None
        };
        // UTM names the UUID each clone gets: the source's is the one a
        // clone must not have.
        let source_uuid = if kind == Branching::Clone {
            let uuid = adapter.query_uuid(
                &Vm {
                    name: src,
                    entry: &source,
                },
                runner,
            );
            Some(uuid.ok_or_else(|| {
                refuse(
                    format!("{src}: {} did not report its UUID", adapter.name()),
                    "open UTM once and allow this terminal to control it (System Settings > Privacy & Security > Automation), then rerun".into(),
                )
            })?)
        } else {
            None
        };
        Ok((frozen, source_uuid))
    }

    fn new_names(
        registry: &Registry,
        src: &str,
        names: &[String],
    ) -> Result<(Vec<String>, Vec<String>), CliError> {
        let (mut todo, mut skipped) = (Vec::new(), Vec::new());
        for name in names {
            let valid = !name.is_empty()
                && !name.starts_with(['.', '-'])
                && name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
            if !valid {
                return Err(refuse(
                    format!("into '{name}': a VM name is letters, digits, '-', '_' and '.'"),
                    "pick a name like dev-2 or review_a".into(),
                ));
            }
            if name == src || todo.contains(name) || skipped.contains(name) {
                return Err(refuse(
                    format!("into '{name}' twice"),
                    "name each new VM once, and not after the source".into(),
                ));
            }
            match registry.vms.get(name) {
                Some(entry) if entry.branched_from.as_deref() == Some(src) => {
                    skipped.push(name.clone());
                }
                Some(_) => {
                    return Err(refuse(
                        format!("into '{name}': a VM of that name is registered"),
                        format!("pick another name, or delete that VM first: tilo-vm vm rm {name}"),
                    ));
                }
                None => todo.push(name.clone()),
            }
        }
        Ok((todo, skipped))
    }

    fn freeze(
        &self,
        runner: &mut dyn Runner,
        registry: &mut Registry,
        src: &str,
        disk: &str,
    ) -> Result<Frozen, CliError> {
        let mut source = registry.vms.get(src).cloned().unwrap_or_default();
        let simple = uuid::Uuid::new_v4().simple().to_string();
        let id = format!("{src}-{}", simple.chars().take(12).collect::<String>());
        let dir = Path::new(disk)
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("bases");
        std::fs::create_dir_all(&dir).map_err(CliError::io(format!("create {}", dir.display())))?;
        let base = dir.join(format!("{id}.qcow2")).display().to_string();
        let permissions = std::fs::metadata(disk)
            .map_err(CliError::io(format!("read {disk}")))?
            .permissions();
        std::fs::rename(disk, &base).map_err(CliError::io(format!("move {disk} to {base}")))?;
        let frozen = Frozen {
            id: id.clone(),
            base: base.clone(),
            disk: disk.to_owned(),
            image: source.image.clone(),
            image_format: source.image_format.clone(),
            permissions,
        };
        let mut read_only = frozen.permissions.clone();
        read_only.set_readonly(true);
        source.image = Some(base.clone());
        source.image_format = None;
        let result = std::fs::set_permissions(&base, read_only)
            .map_err(CliError::io(format!("make {base} read-only")))
            .and_then(|()| {
                hypervisor::overlay_disk(&Vm {
                    name: src,
                    entry: &source,
                })
            })
            .and_then(|create| run_all(runner, &create.into_iter().collect::<Vec<_>>()))
            .and_then(|()| {
                registry.bases.insert(
                    id,
                    Base {
                        disk: base.clone(),
                        image: frozen.image.clone(),
                        image_format: frozen.image_format.clone(),
                        from: src.to_owned(),
                        frozen: chrono::Utc::now()
                            .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    },
                );
                registry.vms.insert(src.to_owned(), source);
                registry.save(&self.registry_path)
            });
        match result {
            Ok(()) => Ok(frozen),
            Err(error) => match self.unfreeze(registry, src, &frozen) {
                Ok(()) => Err(error),
                Err(undo) => Err(refuse(
                    format!("{src}: {error}, and putting its disk back failed: {undo}"),
                    format!("move {base} back to {disk} by hand"),
                )),
            },
        }
    }

    fn unfreeze(
        &self,
        registry: &mut Registry,
        src: &str,
        frozen: &Frozen,
    ) -> Result<(), CliError> {
        remove_if_present(Path::new(&frozen.disk))?;
        std::fs::set_permissions(&frozen.base, frozen.permissions.clone()).map_err(
            CliError::io(format!("restore the permissions of {}", frozen.base)),
        )?;
        std::fs::rename(&frozen.base, &frozen.disk).map_err(CliError::io(format!(
            "move {} to {}",
            frozen.base, frozen.disk
        )))?;
        if registry.bases.remove(&frozen.id).is_none() {
            return Ok(());
        }
        if let Some(entry) = registry.vms.get_mut(src) {
            entry.image.clone_from(&frozen.image);
            entry.image_format.clone_from(&frozen.image_format);
        }
        registry.save(&self.registry_path)
    }

    fn branch_one(
        &self,
        runner: &mut dyn Runner,
        registry: &mut Registry,
        adapter: &dyn Hypervisor,
        (src, name): (&str, &str),
        root_key: &str,
        source_uuid: Option<&str>,
    ) -> Result<Child, CliError> {
        let source = registry.vms.get(src).cloned().unwrap_or_default();
        let (uuid, mac) = loop {
            let (uuid, mac) = hypervisor::new_identity();
            let used = registry
                .vms
                .values()
                .any(|e| e.nic_mac.as_deref() == Some(mac.as_str()));
            if !used {
                break (uuid, mac);
            }
        };
        let key = sshkey::key_path(&self.registry_path, name);
        let mut entry = VmEntry {
            ssh: None,
            key: Some(key.display().to_string()),
            disk: source.disk.as_deref().map(|disk| child_disk(disk, name)),
            ssh_port: forwarded_port::select(registry, &source.hypervisor, None)?,
            uuid: source_uuid.is_none().then_some(uuid),
            nic_mac: Some(mac),
            // A NIC of its own on the host network, unless its source has that turned off.
            lan: (adapter.joins_lan() && source.lan.as_deref() != Some(crate::lan::OFF))
                .then(|| crate::lan::new_mac(|mac| super::lan_taken(registry, mac)))
                .or_else(|| source.lan.clone().filter(|lan| lan == crate::lan::OFF)),
            branched_from: Some(src.to_owned()),
            awaits_host: Some(src.to_owned()),
            template: source_uuid.map_or_else(|| source.template.clone(), |_| Some(src.to_owned())),
            hostname: None,
            mac: None,
            wake: None,
            peer: None,
            ..source.clone()
        };
        copy_key(&sshkey::key_path(&self.registry_path, src), &key)?;
        let vm = Vm {
            name,
            entry: &entry,
        };
        let commands = match adapter.create(&vm, root_key) {
            Ok(commands) => commands,
            Err(error) => {
                remove_child(&entry, &key);
                return Err(error);
            }
        };
        if let Err(error) = run_all(runner, &commands) {
            remove_child(&entry, &key);
            return Err(error);
        }
        let identity = adapter.assign_identity(&vm);
        let mut ran: Vec<String> = commands
            .iter()
            .chain(&identity)
            .map(hypervisor::CommandLine::display)
            .collect();
        let checked = run_all(runner, &identity).and_then(|()| match source_uuid {
            None => Ok(None),
            Some(theirs) => match adapter.query_uuid(&vm, runner) {
                Some(uuid) if uuid != theirs => Ok(Some(uuid)),
                Some(_) => Err(refuse(
                    format!(
                        "{src} into {name}: {} gave {name} {src}'s UUID",
                        adapter.name()
                    ),
                    "update UTM (a clone gets a new UUID), then rerun".into(),
                )),
                None => Err(refuse(
                    format!(
                        "{src} into {name}: {} did not report {name}'s UUID",
                        adapter.name()
                    ),
                    "allow this terminal to control UTM, then rerun".into(),
                )),
            },
        });
        let saved = checked.and_then(|uuid| {
            if uuid.is_some() {
                entry.uuid = uuid;
            }
            registry.vms.insert(name.to_owned(), entry.clone());
            registry.save(&self.registry_path)
        });
        if let Err(error) = saved {
            registry.vms.remove(name);
            let destroy = adapter.destroy(&Vm {
                name,
                entry: &entry,
            });
            ran.extend(destroy.iter().map(hypervisor::CommandLine::display));
            // The VM is being removed because of `error`; that is what to report.
            let _ = run_all(runner, &destroy);
            remove_child(&entry, &key);
            return Err(error);
        }
        Ok(Child {
            name: name.to_owned(),
            entry,
            commands: ran,
        })
    }

    fn branched(
        src: &str,
        registry: &Registry,
        frozen: Option<&Frozen>,
        created: &[Child],
        skipped: &[String],
    ) -> Outcome {
        let names: Vec<&str> = created.iter().map(|c| c.name.as_str()).collect();
        let mut text = format!(
            "{src} branched into {}: copy-on-write, nothing was copied",
            names.join(", ")
        );
        if let Some(frozen) = frozen {
            let _ = write!(
                text,
                "\n  base {}: {src}'s disk as it was, read-only, deleted once no VM uses it\n  {src}: a fresh child of the base at {}, with its own identity as before",
                frozen.base, frozen.disk
            );
        }
        let mut rows = Vec::new();
        for child in created {
            let entry = &child.entry;
            let mut line = format!("\n  {}:", child.name);
            if let Some(disk) = &entry.disk {
                let _ = write!(line, " disk {disk},");
            }
            if let Some(port) = entry.ssh_port {
                let _ = write!(line, " SSH port {port},");
            }
            let _ = write!(
                line,
                " UUID {}, MAC {}",
                entry.uuid.as_deref().unwrap_or("?"),
                entry.nic_mac.as_deref().unwrap_or("?")
            );
            text.push_str(&line);
            rows.push(json!({
                "name": child.name,
                "disk": entry.disk,
                "ssh_port": entry.ssh_port,
                "uuid": entry.uuid,
                "mac": entry.nic_mac,
                "commands": child.commands,
            }));
        }
        if !skipped.is_empty() {
            let _ = write!(
                text,
                "\n  already branched from {src}: {}",
                skipped.join(", ")
            );
        }
        text.push_str(
            "\n  each branch has the whole disk (home, keys, vaults, settings) and the same root SSH key; on first boot it sees its new UUID and gives itself a new device identity, provided the source last booted an image with tilo-device-branch.service (tilo-vm guide concepts)",
        );
        let base = frozen.map_or(
            Value::Null,
            |f| json!({ "id": f.id, "disk": f.base, "users": registry.base_users(&f.base) }),
        );
        let mut outcome = Outcome::new(
            text,
            json!({ "source": src, "base": base, "created": rows, "skipped": skipped }),
        );
        outcome.next = names
            .first()
            .map(|name| format!("tilo-vm vm start {name}"))
            .into_iter()
            .chain(["tilo-vm vm list".to_owned()])
            .collect();
        outcome
    }
}

/// Delete the bases nothing uses any more, oldest link of a chain last:
/// their files and their records. Returns the files deleted; a file that
/// cannot be deleted keeps its record, so the next `vm rm` retries it.
pub(super) fn release_bases(registry: &mut Registry, warnings: &mut Vec<String>) -> Vec<String> {
    let (mut removed, mut stuck) = (Vec::new(), Vec::new());
    loop {
        let unused: Vec<(String, String)> = registry
            .bases
            .iter()
            .filter(|(id, base)| !stuck.contains(*id) && registry.base_users(&base.disk).is_empty())
            .map(|(id, base)| (id.clone(), base.disk.clone()))
            .collect();
        if unused.is_empty() {
            return removed;
        }
        for (id, disk) in unused {
            match remove_base(Path::new(&disk)) {
                Ok(()) => {
                    registry.bases.remove(&id);
                    removed.push(disk);
                }
                Err(error) => {
                    warnings.push(format!(
                        "base {id} is no longer used, but deleting it failed ({error}); the next tilo-vm vm rm retries"
                    ));
                    stuck.push(id);
                }
            }
        }
    }
}

fn remove_base(path: &Path) -> Result<(), CliError> {
    // Windows refuses to delete a read-only file; elsewhere the directory's
    // permissions decide.
    #[cfg(windows)]
    if let Ok(meta) = std::fs::metadata(path) {
        let mut permissions = meta.permissions();
        #[allow(
            clippy::permissions_set_readonly_false,
            reason = "Windows: clears the read-only attribute so the file can be deleted"
        )]
        permissions.set_readonly(false);
        std::fs::set_permissions(path, permissions)
            .map_err(CliError::io(format!("make {} writable", path.display())))?;
    }
    remove_if_present(path).map(|_| ())
}

/// A branch's disk: beside the source's, on the same filesystem as the base.
fn child_disk(source_disk: &str, name: &str) -> String {
    Path::new(source_disk)
        .with_file_name(format!("{name}.qcow2"))
        .display()
        .to_string()
}

/// A branch logs in with the source's root key: the guest's
/// `authorized_keys` is on the disk it got. Each VM has its own copy, so
/// removing one never locks out another; host keys are learnt afresh.
fn copy_key(from: &Path, to: &Path) -> Result<(), CliError> {
    for (from, to) in [
        (from.to_path_buf(), to.to_path_buf()),
        (sshkey::public_path(from), sshkey::public_path(to)),
    ] {
        std::fs::copy(&from, &to).map_err(CliError::io(format!(
            "copy {} to {}",
            from.display(),
            to.display()
        )))?;
    }
    remove_if_present(Path::new(&remote::known_hosts_path(
        &to.display().to_string(),
    )))
    .map(|_| ())
}

fn remove_child(entry: &VmEntry, key: &Path) {
    let mut files: Vec<PathBuf> = entry
        .disk
        .as_deref()
        .map(hypervisor::vm_files)
        .unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect();
    files.extend([
        key.to_path_buf(),
        sshkey::public_path(key),
        PathBuf::from(remote::known_hosts_path(&key.display().to_string())),
    ]);
    for file in files {
        let _ = remove_if_present(&file);
    }
}

fn refuse(what: String, fix: String) -> CliError {
    CliError::Branch { what, fix }
}

fn failed(src: &str, step: &str, error: &CliError, rerun: &str, created: &[String]) -> CliError {
    let state = if created.is_empty() {
        format!("nothing was changed: {src} is as it was")
    } else {
        format!(
            "{} {} made and registered, and they and {src} work",
            created.join(", "),
            if created.len() == 1 { "was" } else { "were" }
        )
    };
    refuse(
        format!("{src}: {step} failed: {error}"),
        format!(
            "{}; {state}. Fix that and rerun, which skips the VMs that exist: {rerun}",
            error.hint()
        ),
    )
}
