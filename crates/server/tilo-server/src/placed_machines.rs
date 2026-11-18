//! A workspace's Linux machine, branched and deleted on the VM machine that
//! keeps it ([`WorkspaceMachines`]), as the workspace is branched or deleted
//! here.
//!
//! The machine that keeps a workspace's VM is the one whose member holds the
//! VM's lease in the workspace ([`PlacedShells::keeper`]). A machine's disk is
//! copied only inside its machine's filesystem, so a branch of a workspace
//! takes its source's machine where the source's VM is kept: that machine's
//! member device, asked over the device action service as this host's own
//! device and in the branch's name, so it enters the branch, makes the
//! branch's machine on its own hosted-VM service ([`environment_machines`]),
//! and from then on holds the branch's lease as it holds every VM's it keeps.
//! A source whose VM nobody keeps has no machine, and then there is nothing to
//! copy. A branch whose machine did not come is deleted as any machine is.
//!
//! A workspace's machine is deleted by the member of the machine keeping it,
//! while the workspace is still there for the member to hold. A workspace
//! whose VM nobody keeps has no machine to delete.
//!
//! A host without a placer that names the hosted-VM service it runs its
//! shells' Linux programs on ([`LocalMachines`]) asks that service directly,
//! on its socket.

use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::workspaces::WorkspaceMachines;
use linux_vm::hosted::control::{self, ENVIRONMENT, Reply, Request, VmRequest};
use tilo_actions::remote::environment_machines::{
    self, BranchMachine, DeleteMachine, MachineBranched,
};
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{InvocationId, InvocationUpdate};
use tilo_peer_state::actions::RemoteDevice;

use crate::placed_shells::{self, PlacedShells};

/// How long a member is given to branch or delete a machine: a source asleep
/// is woken for its copy, which may wait for room.
const MACHINE_PATIENCE: Duration = Duration::from_secs(6 * 60);

/// The machines of the workspaces this host serves, on the VM hosts its placer
/// places them on.
pub struct PlacedMachines {
    placed: Arc<PlacedShells>,
}

impl PlacedMachines {
    #[must_use]
    pub const fn new(placed: Arc<PlacedShells>) -> Self {
        Self { placed }
    }

    /// `host`'s member device, asked in `workspace`'s name as this host's own
    /// device.
    fn member(&self, host: &str, workspace: &str) -> Result<RemoteDevice, String> {
        let member = self
            .placed
            .members()
            .get(host)
            .ok_or_else(|| format!("{host} is no VM machine this host knows"))?;
        let address = placed_shells::member_address(member)?;
        let (action_service, endpoint) = self.placed.asking()?;
        Ok(action_service.remote(endpoint, &address, workspace))
    }
}

impl WorkspaceMachines for PlacedMachines {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        let Some(host) = self.placed.keeper(source)? else {
            return Ok(false);
        };
        let made = BranchMachine {
            from: source.to_owned(),
            account: self.placed.account().to_owned(),
            size: self.placed.size().as_str().to_owned(),
        };
        let branched = self
            .member(&host, branch)
            .and_then(|device| follow(&device, environment_machines::branch_target(), &made))
            .map_err(|words| format!("{host}: {words}"))
            .and_then(|answer| {
                answer
                    .and_then(|value| serde_json::from_value::<MachineBranched>(value).ok())
                    .ok_or_else(|| format!("{host} answered no branch"))
            });
        match branched {
            Ok(branched) => Ok(branched.branched),
            // A machine the member did make is deleted there.
            Err(why) => match self.delete_on(&host, branch) {
                Ok(()) => Err(why),
                Err(left) => Err(format!("{why}; and what it left stays: {left}")),
            },
        }
    }

    fn delete(&self, workspace: &str) -> Result<(), String> {
        match self.placed.keeper(workspace)? {
            Some(host) => self.delete_on(&host, workspace),
            None => Ok(()),
        }
    }

    /// The machines are on the VM machines the placer names, which suspend
    /// their own when they restart: none is on this one.
    fn drain(&self) -> Result<(), String> {
        Ok(())
    }
}

impl PlacedMachines {
    /// Have `host`'s member delete `workspace`'s machine there.
    fn delete_on(&self, host: &str, workspace: &str) -> Result<(), String> {
        let device = self.member(host, workspace)?;
        follow(
            &device,
            environment_machines::delete_target(),
            &DeleteMachine {},
        )
        .map(drop)
        .map_err(|words| format!("{host}: {words}"))
    }
}

impl std::fmt::Debug for PlacedMachines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlacedMachines").finish_non_exhaustive()
    }
}

/// Ask `device` to do `asked` at `target`, and follow it until it ends: what it
/// answered, or the words it failed with.
fn follow(
    device: &RemoteDevice,
    target: RemoteActionTarget,
    asked: &impl serde::Serialize,
) -> Result<Option<serde_json::Value>, String> {
    let serde_json::Value::Object(argument) =
        serde_json::to_value(asked).map_err(|error| error.to_string())?
    else {
        return Err("a machine's request is an object".to_owned());
    };
    let invocation = InvocationId::new();
    let request = InvokeRequest {
        invocation,
        target,
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    };
    // Who asks is the action service's device: the caller here is only what the trait
    // asks for, never sent.
    let caller = Caller::local_app("tilo.server");
    let receipt = placed_shells::settled(device.invoke(&caller, request))?
        .map_err(|error| format!("the VM host refused it: {error}"))?;
    if let ReceiptStatus::Rejected { error } = receipt.status {
        return Err(format!("the VM host refused it: {error}"));
    }
    let until = Instant::now().checked_add(MACHINE_PATIENCE);
    loop {
        if until.is_some_and(|until| Instant::now() >= until) {
            return Err("the VM host did not answer in time".to_owned());
        }
        if let Some(mut change) = device.next_change(&caller, &invocation) {
            while let Err(again) = change.wait_timeout(Duration::from_millis(100)) {
                if until.is_some_and(|until| Instant::now() >= until) {
                    break;
                }
                change = again;
            }
        } else {
            std::thread::sleep(Duration::from_millis(50));
        }
        let Some(state) = device.observe(&caller, &invocation) else {
            continue;
        };
        match state.update {
            InvocationUpdate::Succeeded { output, .. } => return Ok(output.value),
            InvocationUpdate::Failed { error, .. } => return Err(error.message),
            InvocationUpdate::Cancelled { .. } => return Err("it was cancelled".to_owned()),
            InvocationUpdate::Accepted { .. }
            | InvocationUpdate::Progress { .. }
            | InvocationUpdate::Item { .. } => {}
        }
    }
}

/// The machines of the workspaces of a host that runs its shells itself, on
/// the hosted-VM service its environment names
/// ([`ush::hosted_vm::HostedTarget`]): one service, so no placer.
pub struct LocalMachines {
    target: ush::hosted_vm::HostedTarget,
}

impl LocalMachines {
    #[must_use]
    pub const fn new(target: ush::hosted_vm::HostedTarget) -> Self {
        Self { target }
    }

    fn ask(&self, request: &Request) -> Result<Reply, String> {
        let socket = &self.target.socket;
        control::ask(socket, request, MACHINE_PATIENCE)
            .map_err(|error| format!("the hosted-VM service at {}: {error}", socket.display()))?
            .ok_or_else(|| {
                format!(
                    "no hosted-VM service answers at {}, so no machine was changed",
                    socket.display()
                )
            })
    }
}

impl WorkspaceMachines for LocalMachines {
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String> {
        let asked = Request::Branch {
            vm: VmRequest {
                workspace: branch.to_owned(),
                environment: ENVIRONMENT.to_owned(),
                account: self.target.account.clone(),
                size: self.target.size,
            },
            from: source.to_owned(),
        };
        match self.ask(&asked)? {
            Reply::Done => Ok(true),
            Reply::NoVm { .. } => Ok(false),
            Reply::Refused(refusal) => Err(refusal.message),
            Reply::Busy(words) | Reply::Error(words) => Err(words),
            other => Err(format!("the hosted-VM service answered {other:?}")),
        }
    }

    fn delete(&self, workspace: &str) -> Result<(), String> {
        let asked = Request::Delete {
            workspace: workspace.to_owned(),
            environment: ENVIRONMENT.to_owned(),
        };
        match self.ask(&asked)? {
            Reply::Done => Ok(()),
            Reply::Busy(words) | Reply::Error(words) => Err(words),
            other => Err(format!("the hosted-VM service answered {other:?}")),
        }
    }

    /// Have the service take no more wakes and suspend every awake machine,
    /// which it answers once it has, then read back that none is awake.
    fn drain(&self) -> Result<(), String> {
        match control::ask(&self.target.socket, &Request::Drain, MACHINE_PATIENCE) {
            // No service answers, so no machine is awake either.
            Ok(None) => return Ok(()),
            Ok(Some(Reply::Done)) => {}
            Ok(Some(Reply::Busy(words) | Reply::Error(words))) => return Err(words),
            Ok(Some(other)) => {
                return Err(format!("the hosted-VM service answered {other:?}"));
            }
            Err(error) => {
                return Err(format!(
                    "the hosted-VM service at {}: {error}",
                    self.target.socket.display()
                ));
            }
        }
        let Reply::Status { vms, .. } = self.ask(&Request::Status)? else {
            return Err("the hosted-VM service answered no status".to_owned());
        };
        let awake: Vec<String> = vms
            .into_iter()
            .filter(|vm| !matches!(vm.phase.as_str(), "Suspended" | "Off" | "NotCreated"))
            .map(|vm| format!("{} ({})", vm.vm, vm.phase))
            .collect();
        if awake.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "these machines did not suspend: {}",
                awake.join(", ")
            ))
        }
    }
}

impl std::fmt::Debug for LocalMachines {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LocalMachines").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;
    use std::sync::Mutex;

    use linux_vm::hosted::control::{HostLoad, VmStatus};
    use linux_vm::hosted::size::VmSize;

    use super::*;

    type Asked = Arc<Mutex<Vec<Request>>>;

    fn target(socket: std::path::PathBuf) -> LocalMachines {
        LocalMachines::new(ush::hosted_vm::HostedTarget {
            socket,
            account: "person".into(),
            size: VmSize::default(),
            plans: None,
        })
    }

    /// A hosted-VM service on a socket that answers a drain, then reports
    /// its machines in `phases`, noting what it was asked.
    fn service(phases: &'static [&'static str]) -> (tempfile::TempDir, LocalMachines, Asked) {
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("hosted.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let asked = Asked::default();
        let noted = Arc::clone(&asked);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let Some(request) = control::FRAMING
                    .read_value::<_, Request>(&mut stream)
                    .unwrap()
                else {
                    continue;
                };
                let reply = match request {
                    Request::Drain => Reply::Done,
                    Request::Status => Reply::Status {
                        vms: phases
                            .iter()
                            .enumerate()
                            .map(|(at, phase)| VmStatus {
                                vm: format!("ws{at}/default"),
                                account: "person".into(),
                                size: VmSize::default(),
                                phase: (*phase).to_owned(),
                                last_wake_ms: None,
                                last_wake_restored: None,
                                snapshot_bytes: None,
                                resident_mib: None,
                                balloon_mib: 0,
                            })
                            .collect(),
                        load: HostLoad::default(),
                    },
                    _ => Reply::Error("not asked here".into()),
                };
                noted.lock().unwrap().push(request);
                control::FRAMING.write_value(&mut stream, &reply).unwrap();
            }
        });
        (dir, target(socket), asked)
    }

    /// The service is told to drain, and the drain is done only once it
    /// reports every machine asleep.
    #[test]
    fn a_drain_suspends_every_machine_and_reads_back_that_each_is() {
        let (_dir, machines, asked) = service(&["Suspended", "Off"]);
        machines.drain().unwrap();
        assert_eq!(*asked.lock().unwrap(), [Request::Drain, Request::Status]);

        let (_dir, machines, _) = service(&["Suspended", "Running"]);
        let error = machines.drain().unwrap_err();
        assert!(error.contains("ws1/default (Running)"), "{error}");
    }

    /// A server with no hosted-VM service has no machine to suspend.
    #[test]
    fn a_drain_without_a_service_has_nothing_to_suspend() {
        let dir = tempfile::tempdir().unwrap();
        target(dir.path().join("none.sock")).drain().unwrap();
    }
}
