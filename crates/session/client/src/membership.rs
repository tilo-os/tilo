//! A workspace's people, written where the workspace keeps them.
//!
//! The workspace's governance is the only source of who shares it: every change a person makes
//! goes into the workspace's own governance, signed with this device's key, so every member admits
//! it as it admits every change its people make. A change the workspace refuses changes nothing,
//! and the reason goes back to the sheet.
//!
use std::collections::BTreeMap;

use crate::engine::Engine;
use tilo_action_policy::{Change, Grant, Role, WorkspacePolicy};
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{UserId, Workspace, WorkspaceId};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

/// The workspaces a member shows, as a workspace's governance writes into them: the ones it
/// lists, and the policy each carries.
pub trait Workspaces {
    /// Read the workspaces, change them with `edit`, and put them back when it says it
    /// changed something.
    ///
    /// # Errors
    /// What `edit` refused, which leaves the layout as it was.
    fn edit(
        &self,
        edit: &mut dyn FnMut(&mut [Workspace]) -> Result<bool, String>,
    ) -> Result<(), String>;
}

pub struct Membership<'a> {
    engine: &'a Engine,
    workspace_id: WorkspaceId,
}

impl<'a> Membership<'a> {
    /// The people of the workspace `workspace_id` names.
    pub fn of(engine: &'a Engine, workspace_id: WorkspaceId) -> Self {
        Self {
            engine,
            workspace_id,
        }
    }

    /// The changes `edit` made to `workspace` , as the workspace's governance writes them: the
    /// people whose role it set or who it took in, those it removed, and the grants it made and
    /// took away. What the sandbox holds is the workspace's own, not part of who its people are.
    /// Who is already in the workspace is the workspace's roster here that says: naming a role for
    /// someone it does not have adds them, and one it has has their role set.
    pub fn changes(&self, workspace: &Workspace, edit: &PolicyEdit) -> Vec<Change> {
        let mut changes = Vec::new();
        for person in &edit.members {
            changes.push(match workspace.policy.members.get(person) {
                Some(role) if self.has(person) => Change::SetRole {
                    person: person.clone(),
                    role: *role,
                },
                Some(role) => Change::AddMember {
                    person: person.clone(),
                    role: *role,
                },
                None => Change::RemoveMember {
                    person: person.clone(),
                },
            });
        }
        for id in &edit.granted {
            if let Some(grant) = workspace.policy.grants.iter().find(|grant| &grant.id == id) {
                changes.push(Change::Grant(grant.clone()));
            }
        }
        for id in &edit.revoked {
            changes.push(Change::Revoke(id.clone()));
        }
        changes
    }

    /// Whether the workspace has `person` in it, as its own governance here says. A workspace this
    /// device does not hold has nobody here.
    fn has(&self, person: &str) -> bool {
        self.engine
            .people(&self.workspace_id.to_string())
            .is_ok_and(|people| {
                people
                    .iter()
                    .any(|held| held.id.person_id() == Some(person) && !held.removed)
            })
    }

    /// Write `changes` into the workspace's governance as this device, each
    /// signed with its own key: the workspace's admission decides them, here
    /// and on every device that receives them, by the grants of the person
    /// acting. Answers why when the workspace refuses one, so the sheet can
    /// say it; a refused change changes nothing there.
    ///
    /// # Errors
    /// The workspace refused a change, or this device could not publish it.
    pub async fn write(&self, changes: &[Change]) -> Result<(), String> {
        for change in changes {
            self.commit(change).await?;
        }
        Ok(())
    }

    /// [`Self::write`] one change, saying why when the workspace refuses it or cannot publish it.
    pub async fn commit(&self, change: &Change) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Member {
            change: change.clone(),
        })
        .await
    }

    /// Name the workspace `name` as this device: the one name its governance
    /// keeps, which every device and every list of it reads.
    ///
    /// # Errors
    /// The workspace refused it — this device's person is neither its owner nor
    /// a moderator — or this device could not publish it.
    pub async fn rename(&self, name: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Settings(
            SettingsChange::Rename {
                name: name.to_owned(),
            },
        ))
        .await
    }

    /// Take back the workspace's request for the bot whose directory entry is
    /// `entry`, as this device: the workspace's host stops running it from
    /// then on.
    ///
    /// # Errors
    /// The workspace refused the request — this device's person does not
    /// administer it — or this device could not publish it.
    pub async fn withdraw_bot_request(&self, entry: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::WithdrawBotRequest {
            entry: entry.to_owned(),
        })
        .await
    }

    /// Share `node` — one record of the workspace, or one item of it — with its members as `access`
    /// gives, under the share id `share`, as this device: the grants of it that the workspace
    /// admits, bounded by what this person holds at the node.
    ///
    /// # Errors
    /// The workspace refused the share — this device's person does not hold what it would give —
    /// or this device could not publish it.
    pub async fn share(
        &self,
        share: &str,
        node: tilo_action_policy::TreePath,
        access: tilo_action_policy::share::ShareAccess,
    ) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Share {
            share: share.to_owned(),
            node,
            access,
        })
        .await
    }

    /// End the share `share` in the workspace: the grants of it this device's person gave, or all of
    /// them when they may revoke in the workspace.
    ///
    /// # Errors
    /// The workspace refused it, or this device could not publish it.
    pub async fn unshare(&self, share: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Unshare {
            share: share.to_owned(),
        })
        .await
    }

    /// Revoke this device's delegation of its person's work to the server
    /// `server`: the member's own device signed it, so only this one can take
    /// it back, and every holder refuses the server's work for the person from
    /// then on.
    ///
    /// # Errors
    /// This device signed no live delegation to `server`, or the workspace
    /// could not publish the revocation.
    pub async fn revoke_delegation(&self, server: [u8; 32]) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::RevokeWorkDelegation { server })
            .await
    }

    /// Write `command` into the workspace's governance as this device, signed by its ownership,
    /// saying why when the workspace refuses it or cannot publish it.
    async fn command(&self, command: WorkspaceMembershipCommand) -> Result<(), String> {
        let identity = self.engine.identity();
        let request = OwnerCommitRequest {
            workspace: self.workspace_id.to_string(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: uuid::Uuid::new_v4(),
                caller: identity.device_cert.clone(),
                command,
            },
        };
        self.engine
            .owner()
            .commit_workspace_membership_command(
                request,
                identity.signer.as_ref(),
                tilo_clock::wall(),
            )
            .await?;
        // What this device just committed leaves for the workspace's other devices now, rather
        // than with whatever it next writes there.
        tilo_broker_core::owner_service::OwnerService::schedule_publication(
            self.engine,
            self.workspace_id,
        );
        Ok(())
    }
}

/// Write the people `policy` — the workspace's governance — names into `workspace` : the part each
/// of them takes there, when each was admitted, the owner aside, whom the layout's policy names on
/// its own, and the grants it makes, which are the ones the broker decides a person's and an app's
/// actions against here: a grant governs from the device it was written on, and one taken back is
/// gone here as it is everywhere. The windows its devices share with it are among them
/// ([`tilo_desktop_model::window_share`]): a share is the workspace's, so the shell of every device
/// that holds the workspace reads it there, the sharer's own included. Whether anything changed.
pub fn project(workspace: &mut Workspace, policy: &WorkspacePolicy) -> bool {
    let owner = policy.owner.as_deref();
    let members: BTreeMap<UserId, Role> = policy
        .members
        .iter()
        .filter(|(person, _)| owner != Some(person.as_str()))
        .map(|(person, role)| (person.clone(), *role))
        .collect();
    let joined = policy.joined.clone();
    let grants = settled(&policy.grants);
    let held = settled(&workspace.policy.grants);
    if workspace.policy.members == members && workspace.policy.joined == joined && held == grants {
        return false;
    }
    workspace.policy.members = members;
    workspace.policy.joined = joined;
    if held != grants {
        workspace.policy.grants = grants;
    }
    true
}

/// The grants `grants` in a settled order, so two readings of one policy compare equal.
fn settled<'a>(grants: impl IntoIterator<Item = &'a Grant>) -> Vec<Grant> {
    let mut grants: Vec<Grant> = grants.into_iter().cloned().collect();
    grants.sort_by(|a, b| a.id.cmp(&b.id));
    grants
}
