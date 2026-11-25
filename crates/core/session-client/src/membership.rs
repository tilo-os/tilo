//! A workspace's people, written where the workspace keeps them.
//!
//! The workspace's governance is the only source of who shares it: every change a person makes
//! goes into the workspace's own governance, signed with this device's key, so every member admits
//! it as it admits every change its people make. A change the workspace refuses changes nothing,
//! and the reason goes back to the sheet.
//!
use std::collections::BTreeMap;

use crate::engine::Engine;
use tilo_action_policy::PolicyEdit;
use tilo_action_policy::{Change, Grant, Role, WorkspacePolicy};
use tilo_desktop_model::{Workspace, WorkspaceId};
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

/// A membership change written once the workspace answered it.
#[cfg(not(target_arch = "wasm32"))]
pub type Committed<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + Send + 'a>>;
/// A membership change written once the workspace answered it.
#[cfg(target_arch = "wasm32")]
pub type Committed<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<(), String>> + 'a>>;

/// Where a workspace's governance is read and written: who is in it, and each change to its
/// people, signed as this device.
pub trait Roster: collab_types::signer::MaybeSendSync {
    /// The workspace's people, as its governance names them.
    ///
    /// # Errors
    /// The workspace is not one held here.
    fn people(&self, workspace: &str) -> Result<Vec<collab_types::Member>, String>;

    /// Write `command` into `workspace`'s governance as this device, saying why when the workspace
    /// refuses it or it cannot be published.
    fn command(&self, workspace: WorkspaceId, command: WorkspaceMembershipCommand)
    -> Committed<'_>;
}

impl Roster for Engine {
    fn people(&self, workspace: &str) -> Result<Vec<collab_types::Member>, String> {
        Self::people(self, workspace)
    }

    fn command(
        &self,
        workspace: WorkspaceId,
        command: WorkspaceMembershipCommand,
    ) -> Committed<'_> {
        Box::pin(async move {
            let identity = self.identity();
            self.owner()
                .commit_membership(
                    workspace,
                    &identity.device_cert,
                    &command,
                    identity.signer.as_ref(),
                    tilo_clock::wall(),
                )
                .await
                .map(drop)
                .map_err(|error| error.to_string())
        })
    }
}

/// The device's daemon writes the change into the workspace it keeps, signed with the device's
/// key, and forwards it to the device keeping the workspace otherwise
/// ([`crate::workspace_view::MEMBERSHIP`]).
#[cfg(not(target_arch = "wasm32"))]
impl Roster for crate::workspace_view::WorkspaceView {
    fn people(&self, workspace: &str) -> Result<Vec<collab_types::Member>, String> {
        Self::people(self, workspace)
    }

    fn command(
        &self,
        workspace: WorkspaceId,
        command: WorkspaceMembershipCommand,
    ) -> Committed<'_> {
        let command = serde_json::to_value(&command).map_err(|error| error.to_string());
        let asked = command.map(|command| {
            self.ask_holder(
                workspace,
                crate::workspace_view::MEMBERSHIP,
                serde_json::Map::from_iter([("command".to_owned(), command)]),
            )
        });
        Box::pin(async move {
            asked?
                .ok_or("this device reaches no device keeping the workspace")?
                .await
                .map(drop)
        })
    }
}

impl<T: Roster + ?Sized> Roster for std::sync::Arc<T> {
    fn people(&self, workspace: &str) -> Result<Vec<collab_types::Member>, String> {
        (**self).people(workspace)
    }

    fn command(
        &self,
        workspace: WorkspaceId,
        command: WorkspaceMembershipCommand,
    ) -> Committed<'_> {
        (**self).command(workspace, command)
    }
}

pub struct Membership<'a> {
    roster: &'a dyn Roster,
    workspace_id: WorkspaceId,
}

impl<'a> Membership<'a> {
    /// The people of the workspace `workspace_id` names, read and written through `roster`.
    pub fn of(roster: &'a impl Roster, workspace_id: WorkspaceId) -> Self {
        Self {
            roster,
            workspace_id,
        }
    }

    /// The changes `edit` made to `workspace` , as the workspace's governance writes them: the
    /// people whose role it set or who it took in, those it removed, the grants it made and took
    /// away, and what it let the workspace's apps reach or took back. What an app may reach is
    /// the workspace's, so every member's device starts the app with it; one that ends with this
    /// session stays on this device. Who is already in the workspace is the workspace's roster
    /// here that says: naming a role for someone it does not have adds them, and one it has has
    /// their role set.
    pub fn changes(
        &self,
        workspace: &Workspace,
        edit: &PolicyEdit,
    ) -> Vec<WorkspaceMembershipCommand> {
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
        changes.extend(edit.custom_roles.iter().cloned());
        for id in &edit.granted {
            if let Some(grant) = workspace.policy.grants.iter().find(|grant| &grant.id == id) {
                changes.push(Change::Grant(grant.clone()));
            }
        }
        for id in &edit.revoked {
            changes.push(Change::Revoke(id.clone()));
        }
        let mut commands: Vec<_> = changes
            .into_iter()
            .map(|change| WorkspaceMembershipCommand::Member { change })
            .collect();
        for id in &edit.sandbox_granted {
            if let Some(capability) = workspace
                .sandbox
                .iter()
                .find(|capability| &capability.id == id && capability.is_durable())
            {
                commands.push(WorkspaceMembershipCommand::Settings(
                    SettingsChange::GrantSandbox {
                        capability: capability.clone(),
                    },
                ));
            }
        }
        for id in &edit.sandbox_revoked {
            commands.push(WorkspaceMembershipCommand::Settings(
                SettingsChange::RevokeSandbox {
                    capability: id.clone(),
                },
            ));
        }
        commands
    }

    /// Whether the workspace has `person` in it, as its own governance here says. A workspace this
    /// device does not hold has nobody here.
    fn has(&self, person: &str) -> bool {
        self.roster
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
    pub async fn write(&self, changes: &[WorkspaceMembershipCommand]) -> Result<(), String> {
        for change in changes {
            self.command(change.clone()).await?;
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

    /// Remove the member `person` (their user root) and every device of theirs, as this device:
    /// an administrator only by the workspace's owner.
    ///
    /// # Errors
    /// The workspace refused it, saying why, or this device could not publish it.
    pub async fn remove(&self, person: [u8; 32]) -> Result<(), String> {
        self.commit(&Change::RemoveMember {
            person: hex::encode(person),
        })
        .await
    }

    /// Wait until the workspace's host holds every change this device wrote to it over `link`,
    /// the session this membership's changes are written through, or says why it refused one: a
    /// caller that answers with the workspace as its host reads it answers with the change in
    /// it.
    ///
    /// # Errors
    /// The host refused a change, or the session ended before it answered.
    pub async fn settled(&self, link: &crate::Link) -> Result<(), String> {
        use crate::Outbox as _;
        link.barrier_of(&[self.workspace_id])
            .map_err(|closed| closed.to_string())?
            .await
            .map_err(|_| "the session ended before the workspace's host answered".to_owned())?
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

    /// Write `command` into the workspace's governance as this device, signed by its ownership,
    /// saying why when the workspace refuses it or cannot publish it.
    async fn command(&self, command: WorkspaceMembershipCommand) -> Result<(), String> {
        self.roster.command(self.workspace_id, command).await
    }
}

/// Write the people `policy` — the workspace's governance — names into `workspace` : its owner,
/// who may be someone it was handed to since it was founded, the part each other person takes
/// there, when each was admitted, and the grants it makes, which are the ones the broker decides a person's and an app's
/// actions against here: a grant governs from the device it was written on, and one taken back is
/// gone here as it is everywhere. The windows its devices share with it are among them
/// ([`tilo_desktop_model::window_share`]): a share is the workspace's, so the shell of every device
/// that holds the workspace reads it there, the sharer's own included. Whether anything changed.
pub fn project(workspace: &mut Workspace, policy: &WorkspacePolicy) -> bool {
    let owner = policy.owner.as_deref();
    let members: BTreeMap<String, Role> = policy
        .members
        .iter()
        .filter(|(person, _)| owner != Some(person.as_str()))
        .map(|(person, role)| (person.clone(), *role))
        .collect();
    let joined = policy.joined.clone();
    let grants = settled(&policy.grants);
    let held = settled(&workspace.policy.grants);
    if workspace.policy.owner == policy.owner
        && workspace.policy.members == members
        && workspace.policy.joined == joined
        && workspace.policy.custom_roles == policy.custom_roles
        && workspace.policy.assigned_roles == policy.assigned_roles
        && held == grants
    {
        return false;
    }
    workspace.policy.owner.clone_from(&policy.owner);
    workspace.policy.members = members;
    workspace.policy.joined = joined;
    workspace
        .policy
        .custom_roles
        .clone_from(&policy.custom_roles);
    workspace
        .policy
        .assigned_roles
        .clone_from(&policy.assigned_roles);
    if held != grants {
        workspace.policy.grants = grants;
    }
    true
}

/// Write the names the workspace's governance gives its people, `people` (its governed members),
/// into `workspace`: what every surface calls someone who is not here. A person's name is the one
/// they gave themselves, which their server keeps for every workspace of theirs; a removed member's
/// row names nobody who is in it. Whether anything changed.
pub fn project_names(workspace: &mut Workspace, people: &[collab_types::Member]) -> bool {
    workspace.named_people(
        people
            .iter()
            .filter(|member| !member.removed)
            .filter_map(|member| Some((member.id.person_id()?, member.name.as_str()))),
    )
}

/// The grants `grants` in a settled order, so two readings of one policy compare equal.
fn settled<'a>(grants: impl IntoIterator<Item = &'a Grant>) -> Vec<Grant> {
    let mut grants: Vec<Grant> = grants.into_iter().cloned().collect();
    grants.sort_by(|a, b| a.id.cmp(&b.id));
    grants
}
