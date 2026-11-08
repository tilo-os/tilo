//! `vm branch`: new VMs that start as copies of a stopped one.
//!
//! The person sets one box up once (keys, tokens, logins) and branches any
//! number of VMs off it. On libvirt and WHPX the source's disk is frozen as a
//! read-only base in a `bases/` directory beside it, recorded in the
//! registry; the source and every branch then get a copy-on-write qcow2 child
//! of it, so nothing is copied and no VM sees another's writes. A base is
//! deleted once no VM and no other base has it as its backing file
//! ([`release_bases`]), so `vm rm` of the source or of any branch never
//! breaks the others. On UTM each branch is a clone of the source, which UTM
//! makes as an APFS clone of the VM bundle: copy-on-write as well, with no
//! base to keep.
//!
//! Every branch gets its own name, UUID, MAC and SSH port. The UUID is the
//! guest's SMBIOS system UUID (`/sys/class/dmi/id/product_uuid`): the guest
//! sees that it changed and gives itself a new device identity on first boot.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{App, Outcome, remove_if_present};
use crate::cli::BranchArgs;
use crate::config::{Base, Registry, VmEntry};
use crate::error::CliError;
use crate::hypervisor::{self, Branching, Hypervisor, Runner, SystemRunner, Vm, run_all};
use crate::{forwarded_port, remote, sshkey};

/// How long `--stop` waits for the hypervisor to report the source off
/// after its guest shut down.
const STOP_WAIT: Duration = Duration::from_secs(60);

/// A source disk frozen by [`App::freeze`], and what undoes it.
struct Frozen {
    id: String,
    base: String,
    disk: String,
    image: Option<String>,
    image_format: Option<String>,
    permissions: std::fs::Permissions,
}

/// One new VM, made.
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
        self.branch_with(&mut runner, args)
    }

    /// `--stop`: shut the source down the way `vm stop` does, then wait
    /// until its hypervisor reports it off.
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

    /// Branch `args.source` into `args.names` with `runner` running the
    /// hypervisor's commands. Names already registered as branches of the
    /// source are skipped, so a rerun after a failure makes only the rest.
    pub(super) fn branch_with(
        &self,
        runner: &mut dyn Runner,
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
        match adapter.stopped(
            &Vm {
                name: src,
                entry: &source,
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
        let root_key = sshkey::read_public(&sshkey::key_path(&self.registry_path, src))?;
        let (frozen, source_uuid) = self.prepare(
            runner,
            &mut registry,
            adapter.as_ref(),
            (src, &rerun),
            &todo,
        )?;
        let mut created: Vec<Child> = Vec::new();
        for name in &todo {
            let made = self.branch_one(
                runner,
                &mut registry,
                adapter.as_ref(),
                (src, name),
                &root_key,
                source_uuid.as_deref(),
            );
            match made {
                Ok(child) => created.push(child),
                Err(error) => {
                    let mut note = String::new();
                    if let (true, Some(frozen)) = (created.is_empty(), &frozen) {
                        // Nothing was branched: give the source its disk back.
                        if let Err(undo) = self.unfreeze(&mut registry, src, frozen) {
                            note =
                                format!(" ({src} keeps its new disk on the frozen base: {undo})");
                        }
                    }
                    let names: Vec<String> = created.iter().map(|c| c.name.clone()).collect();
                    let mut error = failed(src, &format!("making {name}"), &error, &rerun, &names);
                    if let CliError::Branch { fix, .. } = &mut error {
                        fix.push_str(&note);
                    }
                    return Err(error);
                }
            }
        }
        Ok(Self::branched(
            src,
            &registry,
            frozen.as_ref(),
            &created,
            &skipped,
        ))
    }

    /// The source's adapter, when it can branch and tilo-vm made the VM.
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

    /// What a source needs before its branches are made: on libvirt and
    /// WHPX its disk frozen into a base (each branch's disk path free
    /// first), on UTM the UUID a clone must not have.
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

    /// The names to make and those that already are branches of `src`.
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

    /// Freeze `src`'s disk as a read-only base and give `src` a fresh
    /// child of it at the same path, so its hypervisor definition is
    /// unchanged. On failure everything is put back.
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

    /// Undo [`Self::freeze`]: the base becomes `src`'s writable disk again.
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

    /// Make and register one branch. On failure what it made is removed.
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
            branched_from: Some(src.to_owned()),
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
        // From here the hypervisor has the VM: undo with its destroy.
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
            "\n  each branch has the whole disk (home, keys, vaults, settings) and the same root SSH key; on first boot it sees its new UUID and gives itself a new device identity",
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

/// Remove what a branch that failed left: its disk and the files beside
/// it, and its key.
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
        // Cleanup after the failure being reported; a file that stays is
        // named by vm create's DiskExists if it is in the way of a retry.
        let _ = remove_if_present(&file);
    }
}

fn refuse(what: String, fix: String) -> CliError {
    CliError::Branch { what, fix }
}

/// `step` failed after `created` were made: what still holds, and the rerun.
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
