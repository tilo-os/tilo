//! A VM tilo-vm starts joins this machine's person as one of their devices. Before its first
//! boot `tilo device add` makes a one-use ticket, which the VM gets as the systemd credential
//! `tilo.ticket` and spends; `tilo device add` then names the device that joined, and tilo-vm
//! records it as the device it asks from then on. A machine with no person of its own (CI, a
//! fresh builder) gets a throwaway one under tilo-vm's state directory first.

use std::io::{BufRead as _, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde_json::Value;

use super::{App, Resolved};
use crate::config::{self, DeviceRoute};
use crate::error::CliError;
use crate::hypervisor::{self, run_all};
use crate::remote::{Machine, own_env};
use crate::sshkey;

/// How long a booting VM has to spend its ticket.
const JOIN_WAIT: Duration = Duration::from_secs(300);

/// A ticket handed to a VM, and the `tilo device add` that waits for it to be spent.
pub(super) struct Joining {
    child: Child,
    said: mpsc::Receiver<String>,
    workspace: String,
}

impl Drop for Joining {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl App {
    /// The person this machine adds VMs as: its own login, else tilo-vm's throwaway one, made
    /// now when there is none.
    fn adding_machine(&self) -> Result<Machine, CliError> {
        if let Some(machine) = &self.machine {
            return Ok(machine.clone());
        }
        if let Some(machine) = Machine::here()? {
            return Ok(machine);
        }
        let dir = config::own_identity_dir(&self.registry_path);
        if let Some(machine) = Machine::own(&dir)? {
            return Ok(machine);
        }
        let output = Command::new("tilo")
            .args(["login", "--new-identity", "--json"])
            .envs(own_env(&dir))
            .output()
            .map_err(|error| CliError::NotLoggedIn {
                detail: format!("could not run tilo login --new-identity: {error}"),
            })?;
        if !output.status.success() {
            return Err(CliError::NotLoggedIn {
                detail: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            });
        }
        if !self.json {
            eprintln!(
                "tilo-vm: this computer has no Tilo login, so tilo-vm made a person of its own for its VMs in {}",
                dir.display()
            );
        }
        Machine::own(&dir)?.ok_or_else(|| CliError::NotLoggedIn {
            detail: format!(
                "tilo login --new-identity left no identity in {}",
                dir.display()
            ),
        })
    }

    /// Give `vm` a ticket for its next boot, when it is no device yet and its hypervisor has a
    /// credential channel to pass one in.
    pub(super) fn begin_joining(&mut self, vm: &Resolved) -> Result<Option<Joining>, CliError> {
        let adapter = Self::adapter(vm)?;
        if vm.entry.device.is_some() || adapter.capabilities().credentials.is_none() {
            return Ok(None);
        }
        let machine = self.adding_machine()?;
        let mut child = Command::new("tilo")
            .args(["device", "add", "--json"])
            .envs(machine.env())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(CliError::io("run tilo device add"))?;
        let stdout = child.stdout.take().ok_or_else(|| {
            CliError::io("read tilo device add")(std::io::ErrorKind::BrokenPipe.into())
        })?;
        let (tell, said) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if tell.send(line).is_err() {
                    break;
                }
            }
        });
        let joining = Joining {
            child,
            said,
            workspace: machine.workspace,
        };
        let ticket = joining
            .next("ticket", Duration::from_secs(60))?
            .ok_or_else(|| join_failed(vm, "tilo device add printed no ticket"))?;
        self.update_entry(&vm.name, |entry| entry.ticket = Some(ticket))?;
        self.redefine_for_ticket(vm)?;
        Ok(Some(joining))
    }

    /// Wait for the VM to spend its ticket and record the device it is.
    pub(super) fn finish_joining(
        &mut self,
        vm: &Resolved,
        joining: &Joining,
    ) -> Result<Resolved, CliError> {
        let device = joining.next("device", JOIN_WAIT)?.ok_or_else(|| {
            join_failed(
                vm,
                &format!(
                    "it did not join within {}s of starting",
                    JOIN_WAIT.as_secs()
                ),
            )
        })?;
        let workspace = joining.workspace.clone();
        self.update_entry(&vm.name, |entry| {
            entry.ticket = None;
            entry.device = Some(DeviceRoute {
                device,
                workspace,
                addresses: Vec::new(),
                relay: None,
            });
        })?;
        let vm = self.resolve_vm(Some(&vm.name))?;
        self.redefine_for_ticket(&vm)?;
        Ok(vm)
    }

    /// A libvirt domain carries its OEM strings in its definition, so it is defined again
    /// whenever the ticket comes or goes; the other adapters pass it at each start.
    fn redefine_for_ticket(&mut self, vm: &Resolved) -> Result<(), CliError> {
        if vm.entry.hypervisor != "libvirt" {
            return Ok(());
        }
        let vm = self.resolve_vm(Some(&vm.name))?;
        let key = sshkey::key_path(&self.registry_path, &vm.name);
        let root_key = sshkey::read_public(&key)?;
        let commands =
            hypervisor::adapter("libvirt")?.redefine(&vm.vm(), &root_key, &mut self.runner)?;
        run_all(&mut self.runner, &commands)
    }
}

impl Joining {
    /// The value of `field` in the next line `tilo device add` prints, within `wait`.
    fn next(&self, field: &str, wait: Duration) -> Result<Option<String>, CliError> {
        let Ok(line) = self.said.recv_timeout(wait) else {
            return Ok(None);
        };
        let value: Value =
            serde_json::from_str(&line).map_err(|error| CliError::CommandFailed {
                command: "tilo device add --json".into(),
                status: "unreadable answer".into(),
                stderr: format!("{error}: {line}"),
            })?;
        Ok(value.get(field).and_then(Value::as_str).map(str::to_owned))
    }
}

fn join_failed(vm: &Resolved, why: &str) -> CliError {
    CliError::Unreachable {
        vm: vm.name.clone(),
        detail: format!("it was to join this machine's person as a device, but {why}"),
        debug: vec![format!("tilo-vm vm serial {}", vm.name)],
    }
}
