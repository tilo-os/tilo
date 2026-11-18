//! A workspace's Linux machine, which whatever runs the workspaces' machines
//! keeps beside its records ([`WorkspaceMachines`]): a branch takes its
//! source's, and a deleted workspace's goes before the workspace does.
//!
//! Both happen inside the operation that asks for them, never after it from a
//! notice. A branch's machine is made before the branch is the person's and
//! before its maker hears it is there: a command run in a branch whose machine
//! was still to come would get a new machine, and the copy would then be
//! refused. A workspace is deleted only once its machine is: a host that
//! cannot be reached, or refuses, fails the deletion, which can be asked
//! again, so no workspace is gone with its disk left behind.

use std::sync::Arc;

use tilo_lock::RwLockRecover as _;

use super::Workspaces;

/// What runs the Linux machines of the workspaces this host governs, as a
/// branch takes its source's and a deletion takes the workspace's.
pub trait WorkspaceMachines: Send + Sync {
    /// Make `branch`'s machine a copy of `source`'s as it is now: `true` once
    /// it is there, `false` when `source` has no machine, so there is nothing
    /// to copy and the branch's starts as any new workspace's does.
    ///
    /// # Errors
    ///
    /// The machine could not be copied, named with why.
    fn branch(&self, source: &str, branch: &str) -> Result<bool, String>;

    /// Delete `workspace`'s machine, its disk and its snapshot; done when it
    /// has none. Asked while the workspace still exists.
    ///
    /// # Errors
    ///
    /// The machine's host could not be reached, or refused: the machine may
    /// still be there.
    fn delete(&self, workspace: &str) -> Result<(), String>;

    /// Suspend every machine awake on this device and take no more wakes,
    /// before it restarts: done once each one is suspended. Machines this
    /// device only places on others are theirs to suspend.
    ///
    /// # Errors
    ///
    /// The machines' service could not be reached, or a machine is still
    /// awake, named so.
    fn drain(&self) -> Result<(), String>;
}

impl Workspaces {
    /// Have `machines` branch a source's machine into each branch made here,
    /// and delete each workspace's machine before the workspace.
    pub fn machines(&self, machines: Arc<dyn WorkspaceMachines>) {
        *self.inner.machines.write_recover() = Some(machines);
    }

    /// Make `branch`'s machine a copy of `source`'s, where machines are run
    /// for this host's workspaces.
    ///
    /// # Errors
    ///
    /// Why the machine did not come.
    #[cfg_attr(
        not(feature = "agents"),
        expect(dead_code, reason = "only a host that branches copies a machine")
    )]
    pub(super) async fn branch_machine(&self, source: &str, branch: &str) -> Result<(), String> {
        let Some(machines) = self.inner.machines.read_recover().clone() else {
            return Ok(());
        };
        let asked = (source.to_owned(), branch.to_owned());
        let branched = tokio::task::spawn_blocking(move || machines.branch(&asked.0, &asked.1))
            .await
            .map_err(|error| error.to_string())??;
        if !branched {
            tracing::info!(%source, %branch, "the source has no Linux machine yet, so its branch starts without one");
        }
        Ok(())
    }

    /// Delete `workspace`'s machine, where machines are run for this host's
    /// workspaces.
    ///
    /// # Errors
    ///
    /// The machine may still be there, named so.
    pub(super) async fn delete_machine(&self, workspace: &str) -> Result<(), String> {
        let Some(machines) = self.inner.machines.read_recover().clone() else {
            return Ok(());
        };
        let workspace = workspace.to_owned();
        tokio::task::spawn_blocking(move || machines.delete(&workspace))
            .await
            .map_err(|error| error.to_string())?
            .map_err(|why| {
                format!(
                    "its Linux machine was not deleted, so the workspace stays; ask again: {why}"
                )
            })
    }
}

impl Workspaces {
    /// Suspend the machines awake on this device, where it keeps any, and
    /// answer once every one is suspended.
    ///
    /// # Errors
    ///
    /// A machine is still awake, or their service did not answer.
    pub async fn drain_machines(&self) -> Result<(), String> {
        let Some(machines) = self.inner.machines.read_recover().clone() else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || machines.drain())
            .await
            .map_err(|error| error.to_string())?
    }
}

#[cfg(test)]
#[path = "machines_tests.rs"]
mod tests;
