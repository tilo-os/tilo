//! A person's servers update one at a time.
//!
//! Each server stages its OS updates by itself, but restarts into one only
//! while it holds its person's admin workspace's [`UPDATE_LEASE`]: the same
//! lease every holder of a workspace takes exactly-once work under, so at
//! most one of the person's servers is down for an update at a time. Holding
//! it, the server first checks that each workspace it holds keeps the copies
//! its policy wants without this server, and that none of its copies is still
//! catching up (it waits otherwise, and asks again later), then drains: it suspends the workspaces'
//! Linux machines running on it and waits until each one is, and only then
//! takes and renews no lease of any workspace and lets go of the claims it
//! has, so their other holders take that work over. A machine's lease is let
//! go of after the machine is asleep, so whoever takes it never finds it
//! still running here. Only then does it answer that the machine may
//! restart.
//!
//! The lease is let go of after the restart, once the new generation has
//! passed the server's health verdict ([`booted`]). A generation that fails
//! it is taken back by the boot loader, and the server comes up on the old
//! one still holding the lease: the next server never starts the update that
//! failed, so a bad update halts the rollout by itself. The server keeps the
//! lease, renewing it while it is up, until an update boots healthy; it never
//! restarts into the generation that failed again.
//!
//! The update service, `tilo-server-update-apply` as root, asks this daemon
//! over a socket in its state root ([`SOCKET`]), through `tilo-server
//! rollout`, since the daemon itself may not restart the machine. A desktop
//! never takes the lease: desktops holding a workspace together restart when
//! their people do.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde::{Deserialize, Serialize};
use tilo_user_identity::DeviceCert;

use crate::servers;
use crate::workspaces::Workspaces;

/// The lease, in the person's admin workspace, a server holds while it
/// restarts into an update.
pub const UPDATE_LEASE: &str = "tilo.servers.update";
/// How long one claim of [`UPDATE_LEASE`] lasts: longer than a restart takes,
/// including the boot loader's tries of a generation that fails, so the
/// lease outlives the restart of the server holding it.
pub const UPDATE_LEASE_TTL: Duration = Duration::from_secs(2 * 60 * 60);
/// The file in the state root naming the image a server restarted into while
/// it held [`UPDATE_LEASE`].
const PENDING: &str = "update-pending";

/// What the update service asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "ask", rename_all = "snake_case")]
pub enum Request {
    /// May this machine restart into `target` (a staged image) now?
    Prepare { target: String },
    /// This machine booted `image` and has passed its health verdict.
    Booted { image: String },
    /// After a healthy boot: may its rollback floor move, as its TPM states?
    Floor(crate::servers::floor::Asked),
}

/// What the daemon answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "answer", rename_all = "snake_case")]
pub enum Answer {
    /// Restart now: this server holds the update lease and has drained.
    Restart,
    /// Do not restart yet, for the reason said.
    Wait { why: String },
    /// The update booted healthy and the lease is let go of.
    Released,
    /// The update did not boot (`booted` came up instead of `target`): the
    /// lease stays held, so no other server starts it.
    Halted { target: String, booted: String },
    /// No update was under way.
    Nothing,
    /// The approvals another of the person's devices signed for the floor to
    /// move; none, and why, while nobody has.
    Floor {
        approvals: Vec<tilo_rollback::approval::Approval>,
        why: String,
    },
}

/// What a server does around a restart. The daemon's is [`ServerMachine`];
/// tests stand in for it.
pub trait Machine {
    /// Take [`UPDATE_LEASE`] or push its deadline out. Answers whether this
    /// server holds it.
    fn hold_update_lease(&self) -> impl Future<Output = Result<bool, String>> + Send;
    /// Let [`UPDATE_LEASE`] go.
    fn release_update_lease(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// The workspaces this server holds whose copies would fall short while
    /// it restarts; empty when every one keeps its copies elsewhere.
    fn short_of_copies(&self) -> impl Future<Output = Result<Vec<String>, String>> + Send;
    /// Take and renew no lease from now on, and let go of the claims held.
    fn hand_off_leases(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// Suspend the workspaces' Linux machines running on this server and
    /// take no more wakes; done once every one is suspended.
    fn suspend_machines(&self) -> impl Future<Output = Result<(), String>> + Send;
    /// The image this server restarted into under the lease, if it did.
    fn pending(&self) -> Option<String>;
    /// Keep (`Some`) or forget (`None`) the image restarted into.
    ///
    /// # Errors
    /// It could not be written.
    fn set_pending(&self, target: Option<&str>) -> Result<(), String>;
}

/// Whether this machine may restart into `target` now, draining it when it
/// may: the lease is taken first, the copies checked under it, and the work
/// handed off last.
///
/// # Errors
/// The lease, the check or the drain could not be written.
pub async fn prepare(machine: &(impl Machine + Sync), target: &str) -> Result<Answer, String> {
    if let Some(failed) = machine.pending()
        && failed == target
    {
        // Still on the old generation with this one pending: it was taken
        // back. Booting it again would spend the machine's tries again.
        machine.hold_update_lease().await?;
        return Ok(Answer::Wait {
            why: format!(
                "{target} did not boot healthy on this server; waiting for a newer update"
            ),
        });
    }
    if !machine.hold_update_lease().await? {
        return Ok(Answer::Wait {
            why: "another of this person's servers is updating".into(),
        });
    }
    // A server still holding the lease for an update that failed keeps it,
    // so the rollout stays halted until this one boots healthy.
    let halted = machine.pending();
    let short = machine.short_of_copies().await?;
    if !short.is_empty() {
        if halted.is_none() {
            machine.release_update_lease().await?;
        }
        return Ok(Answer::Wait {
            why: format!(
                "these workspaces would have too few copies while this server restarts: {}",
                short.join(", ")
            ),
        });
    }
    machine.set_pending(Some(target))?;
    if let Err(why) = machine.suspend_machines().await {
        machine.set_pending(halted.as_deref())?;
        if halted.is_none() {
            machine.release_update_lease().await?;
        }
        return Ok(Answer::Wait {
            why: format!("the Linux machines on this server did not all suspend: {why}"),
        });
    }
    machine.hand_off_leases().await?;
    Ok(Answer::Restart)
}

/// This machine booted `image`, judged healthy: an update it restarted into
/// under the lease is done, and the lease goes to the next server. A machine
/// that came up on another image keeps the lease.
///
/// # Errors
/// The lease could not be written.
pub async fn booted(machine: &(impl Machine + Sync), image: &str) -> Result<Answer, String> {
    let Some(target) = machine.pending() else {
        return Ok(Answer::Nothing);
    };
    if target != image {
        machine.hold_update_lease().await?;
        return Ok(Answer::Halted {
            target,
            booted: image.to_owned(),
        });
    }
    machine.release_update_lease().await?;
    machine.set_pending(None)?;
    Ok(Answer::Released)
}

/// What the device action `action`, asked of this computer's daemon with
/// `args`, does to `machine`: [`prepare`] and [`booted`] by name, which is how
/// the automatic update path takes its turn among the person's servers.
///
/// # Errors
/// The action's arguments name nothing it is about, the action is not one of
/// these, or the lease could not be written.
pub async fn answer_action<M: Machine + Sync>(
    machine: &M,
    action: &str,
    args: &serde_json::Value,
) -> Result<Answer, String> {
    let named = |field: &str| {
        args.get(field)
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
            .filter(|said| !said.trim().is_empty())
            .ok_or_else(|| format!("{action} names no {field}"))
    };
    match action {
        "prepare" => prepare(machine, &named("target")?).await,
        "booted" => booted(machine, &named("image")?).await,
        _ => Err(format!("this device has no update action {action:?}")),
    }
}

/// Keep in `dir` the approvals another of the person's devices signed for this
/// server's floor to move, where `tilo-rollback-confirm` spends them.
///
/// # Errors
/// The approvals could not be written.
pub fn store_approvals(
    dir: &Path,
    approvals: &[tilo_rollback::approval::Approval],
) -> Result<Vec<PathBuf>, String> {
    tilo_rollback::approval::store(dir, approvals).map_err(|error| format!("{error:#}"))
}

/// Why `workspace` (whose copies stand at `status`) may not lose `own` for
/// a restart: a holder still lacks something another has, or its owners
/// chose how many copies it keeps and `own` is one of too few current copies
/// of a kind for that to hold without it. `None` when the restart leaves the
/// policy kept. A workspace whose owners chose nothing keeps the default,
/// which asks no copy beyond the server: a person with one server still
/// updates it, and the workspace is back with it after the restart.
fn short_without(status: &tilo_peer_state::CopyStatus, own: &[u8; 32]) -> Option<String> {
    use tilo_peer_state::{CopyRole, HolderCopy, Shortfall};
    if status
        .shortfall
        .iter()
        .any(|short| matches!(short, Shortfall::Holder { .. }))
    {
        return Some("a copy is still catching up".to_owned());
    }
    if !status.chosen {
        return None;
    }
    [CopyRole::KeyHolder, CopyRole::Store]
        .into_iter()
        .find_map(|role| {
            let current = |holder: &&HolderCopy| holder.role == role && holder.current;
            let with = status.holders.iter().filter(current).count();
            let without = status
                .holders
                .iter()
                .filter(current)
                .filter(|holder| holder.device != *own)
                .count();
            let wanted = usize::try_from(status.policy.wanted(role)).unwrap_or(usize::MAX);
            (without < with && without < wanted).then(|| {
                let kind = match role {
                    CopyRole::KeyHolder => "key holders",
                    CopyRole::Store => "stores",
                };
                format!("it wants {wanted} current {kind} and keeps {without} without this server")
            })
        })
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH
        .checked_add(Duration::from_nanos(tilo_clock::unix_now_ns()))
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// A server enrolled into its person's admin workspace.
#[derive(Clone)]
pub struct ServerMachine {
    workspaces: Workspaces,
    state_root: PathBuf,
}

impl ServerMachine {
    #[must_use]
    pub const fn new(workspaces: Workspaces, state_root: PathBuf) -> Self {
        Self {
            workspaces,
            state_root,
        }
    }

    /// Suspend the workspaces' Linux machines and hand their work off now,
    /// as a restart for an update does, without taking the update lease: what
    /// an owner asks for before taking a server out of service. Answers how
    /// many workspaces this server holds.
    ///
    /// # Errors
    /// The machines did not all suspend, or a lease could not be let go of.
    pub async fn drain_now(&self) -> Result<usize, String> {
        let held = self.workspaces.list().await.len();
        self.suspend_machines().await?;
        self.hand_off_leases().await?;
        Ok(held)
    }

    /// Where this server keeps what it holds, the image it restarted into among
    /// them.
    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// Ask the person's devices, in their admin workspace, for this server's
    /// rollback floor to move past `asked.generation`: the approvals they
    /// signed, or why none came.
    ///
    /// # Errors
    /// The person's admin workspace could not be read.
    pub async fn floor(
        &self,
        asked: crate::servers::floor::Asked,
    ) -> Result<Result<Vec<tilo_rollback::approval::Approval>, String>, String> {
        crate::servers::floor::ask(&self.workspaces, asked).await
    }

    fn device(&self) -> &DeviceCert {
        self.workspaces.device_cert()
    }

    fn admin(&self) -> String {
        servers::person_workspace_id(&self.device().user_public_key)
    }

    fn pending_path(&self) -> PathBuf {
        self.state_root.join(PENDING)
    }

    async fn release(&self, workspace: &str, lease: &str) -> Result<(), String> {
        let device = self.device().clone();
        self.workspaces
            .author_own_workspace_state(workspace, &device, |peer| {
                peer.workspace_mut().release_lease(&device, lease, now())
            })
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }
}

impl Machine for ServerMachine {
    async fn hold_update_lease(&self) -> Result<bool, String> {
        let device = self.device().clone();
        self.workspaces
            .author_own_workspace_state(&self.admin(), &device, |peer| {
                peer.workspace_mut().take_lease(
                    &device,
                    UPDATE_LEASE,
                    UPDATE_LEASE_TTL,
                    tilo_peer_state::unranked(),
                    now(),
                )
            })
            .await
            .map(|claim| claim.is_some())
            .map_err(|error| error.to_string())
    }

    async fn release_update_lease(&self) -> Result<(), String> {
        self.release(&self.admin(), UPDATE_LEASE).await
    }

    async fn short_of_copies(&self) -> Result<Vec<String>, String> {
        let own = self.device().device_public_key;
        let mut short = Vec::new();
        for summary in self.workspaces.list().await {
            let Some(state) = self.workspaces.state(&summary.workspace).await else {
                continue;
            };
            if let Some(why) = short_without(&state.workspace().copies(), &own) {
                short.push(format!("{} ({why})", summary.workspace));
            }
        }
        Ok(short)
    }

    async fn hand_off_leases(&self) -> Result<(), String> {
        self.workspaces.drain();
        let own = self.device().device_public_key;
        let admin = self.admin();
        let now = now();
        for summary in self.workspaces.list().await {
            let Some(state) = self.workspaces.state(&summary.workspace).await else {
                continue;
            };
            let claimed = state
                .workspace()
                .leases(now)
                .into_iter()
                .filter(|lease| lease.claims.iter().any(|claim| claim.device == own))
                .filter(|lease| !(summary.workspace == admin && lease.id == UPDATE_LEASE))
                .map(|lease| lease.id)
                .collect::<Vec<_>>();
            for lease in claimed {
                self.release(&summary.workspace, &lease).await?;
            }
        }
        Ok(())
    }

    async fn suspend_machines(&self) -> Result<(), String> {
        self.workspaces.drain_machines().await
    }

    fn pending(&self) -> Option<String> {
        std::fs::read_to_string(self.pending_path())
            .ok()
            .map(|target| target.trim().to_owned())
            .filter(|target| !target.is_empty())
    }

    fn set_pending(&self, target: Option<&str>) -> Result<(), String> {
        let path = self.pending_path();
        match target {
            Some(target) => tilo_files::native::atomic::write_with_mode(
                &path,
                format!("{target}\n").as_bytes(),
                0o600,
            )
            .map_err(|error| error.to_string()),
            None => match std::fs::remove_file(&path) {
                Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
                    Err(error.to_string())
                }
                _ => Ok(()),
            },
        }
    }
}

#[cfg(test)]
mod tests;
