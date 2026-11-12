//! A workspace's people, written where the workspace keeps them.
//!
//! The workspace's governance is the only source of who shares it: every change a person makes
//! goes into the workspace's own governance, signed with this device's key, so every member admits
//! it as it admits every change its people make. A change the workspace refuses changes nothing,
//! and the reason goes back to the sheet.
//!
//! A package the person opens belongs there too: the workspace's governance gives it authorship of
//! its own records when they may change the workspace ([`authorship`]). A workspace only one desk
//! holds has no governance to ask, and its owner gives the grant there directly.

use std::collections::BTreeMap;

use crate::engine::Engine;
use tilo_action_policy::{Change, Grant, Role, Subject, WorkspacePolicy};
use tilo_desktop_model::grants::PolicyEdit;
use tilo_desktop_model::{UserId, Workspace, WorkspaceId};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

/// The desk a participant shows, as a workspace's governance writes into it: the workspaces it
/// lists, and the policy each of them carries.
pub trait Workspaces {
    /// Read the desk's workspaces, change them with `edit`, and put them back when it says it
    /// changed something.
    ///
    /// # Errors
    /// What `edit` refused, which leaves the desk as it was.
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
                    .any(|held| held.person == person && !held.removed)
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
    pub fn write(&self, changes: &[Change]) -> Result<(), String> {
        for change in changes {
            self.commit(change)?;
        }
        Ok(())
    }

    /// [`Self::write`] one change, saying why when the workspace refuses it or cannot publish it.
    pub fn commit(&self, change: &Change) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Member {
            change: change.clone(),
        })
    }

    /// Take back the workspace's request for the bot whose directory entry is
    /// `entry`, as this device: the workspace's host stops running it from
    /// then on.
    ///
    /// # Errors
    /// The workspace refused the request — this device's person does not
    /// administer it — or this device could not publish it.
    pub fn withdraw_bot_request(&self, entry: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::WithdrawBotRequest {
            entry: entry.to_owned(),
        })
    }

    /// Share `node` — one record of the workspace, or one item of it — with its members as `access`
    /// gives, under the share id `share`, as this device: the grants of it that the workspace
    /// admits, bounded by what this person holds at the node.
    ///
    /// # Errors
    /// The workspace refused the share — this device's person does not hold what it would give —
    /// or this device could not publish it.
    pub fn share(
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
    }

    /// End the share `share` in the workspace: the grants of it this device's person gave, or all of
    /// them when they may revoke in the workspace.
    ///
    /// # Errors
    /// The workspace refused it, or this device could not publish it.
    pub fn unshare(&self, share: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::Unshare {
            share: share.to_owned(),
        })
    }

    /// Write `command` into the workspace's governance as this device, signed by its custody,
    /// saying why when the workspace refuses it or cannot publish it.
    fn command(&self, command: WorkspaceMembershipCommand) -> Result<(), String> {
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
            .map(|_receipt| ())
    }

    /// Let `app` author its own records in the workspace, as this device: the
    /// grant a person who may change the workspace gives an app they open
    /// there ([`tilo_action_policy::roles::app_authorship`]), which the
    /// workspace decides and every device of it receives like any other
    /// change.
    ///
    /// # Errors
    /// The workspace refused the grant — this device's person does not
    /// administer it — or this device could not publish it.
    pub fn grant_app_authorship(&self, app: &str) -> Result<(), String> {
        self.command(WorkspaceMembershipCommand::GrantAppAuthorship {
            app: app.to_owned(),
        })
    }
}

/// Let `app` author its own records in `workspace`, as the person who opens it
/// there: the workspace's governance gives the grant when they administer it
/// ([`crate::engine::Engine::administers`]), and the desk takes it from there
/// like the rest of the workspace's grants ([`project`]), so the launch that
/// follows hands the app its authorship. A workspace this device's governance
/// does not hold — one that lives only on this desk, or a session that holds
/// none — has nothing to ask, and its own policy takes the grant from the
/// owner. Nothing is written while the workspace already makes the app an
/// author.
///
/// # Errors
/// The workspace's governance refused the grant, or a desk-only workspace has
/// no owner to give it.
pub fn authorship(
    engine: Option<&Engine>,
    desk: &dyn Workspaces,
    workspace: &str,
    app: &str,
) -> Result<(), String> {
    let id = WorkspaceId::parse(workspace).ok_or("the launch names no workspace")?;
    let authors = |policy: &WorkspacePolicy| {
        policy
            .grants
            .iter()
            .any(|grant| is_authorship_of(grant, app))
    };
    if let Some(engine) = engine.filter(|engine| engine.holds(id)) {
        let policy = engine.policy(workspace)?;
        // The grant is the workspace's to give, and this device gives it where its person
        // administers the workspace: a member whose role only lets them write cannot change its
        // people, and asks for nothing here. The workspace's own devices grant it, and what they
        // grant arrives in the projection below like the rest of its grants.
        if !authors(&policy) && engine.administers(workspace) {
            Membership::of(engine, id).grant_app_authorship(app)?;
        }
        let policy = engine.policy(workspace)?;
        edit_workspace(desk, id, |shown| Ok(project(shown, &policy)))?;
    } else {
        // A workspace that lives only on this desk holds no governance here, so the desk's own
        // policy is where its authorship goes.
        let now = tilo_clock::wall()
            .duration_since(tilo_clock::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs());
        edit_workspace(desk, id, |shown| author_on_desk(shown, app, now))?;
    }
    Ok(())
}

/// Let `app` author its records in `workspace` , which lives only on this desk: the owner, whose
/// authority is structural, gives the grant, and no other device is asked because no other device
/// holds the workspace. Whether the desk changed.
fn author_on_desk(workspace: &mut Workspace, app: &str, now: u64) -> Result<bool, String> {
    if workspace.app_is_author_at(app, now) {
        return Ok(false);
    }
    let Some(owner) = workspace.owner().cloned() else {
        return Err("the workspace has no one to give authorship".to_owned());
    };
    let grant = tilo_action_policy::roles::app_authorship(
        &workspace.id().to_string(),
        app,
        Subject::person(&owner),
    );
    workspace
        .govern_as_at(&owner, Change::Grant(grant), now)
        .map_err(|refusal| refusal.to_string())?;
    Ok(true)
}

/// Write the people `policy` — the workspace's governance — names into `workspace` : the part each
/// of them takes there, when each was admitted, the owner aside, whom the desk's policy names on
/// its own, the agents it admits, each with the grants governance gave it, and the apps it lets
/// author their own records there. The grants a person gives to a person on a node below the
/// workspace, and the ones a consent or an invite makes for this session, are the desk's own and
/// stay. Whether anything changed.
pub fn project(workspace: &mut Workspace, policy: &WorkspacePolicy) -> bool {
    let owner = policy.owner.as_deref();
    let members: BTreeMap<UserId, Role> = policy
        .members
        .iter()
        .filter(|(person, _)| owner != Some(person.as_str()))
        .map(|(person, role)| (person.clone(), *role))
        .collect();
    let joined = policy.joined.clone();
    let agents = picked_grants(&policy.grants, |grant| {
        matches!(grant.subject, Subject::Agent(_))
    });
    let authored = picked_grants(&policy.grants, is_authorship);
    let held = picked_grants(&workspace.policy.grants, |grant| {
        matches!(grant.subject, Subject::Agent(_))
    });
    let held_authored = picked_grants(&workspace.policy.grants, is_authorship);
    if workspace.policy.members == members
        && workspace.policy.joined == joined
        && held == agents
        && held_authored == authored
    {
        return false;
    }
    workspace.policy.members = members;
    workspace.policy.joined = joined;
    if held != agents {
        workspace
            .policy
            .grants
            .retain(|grant| !matches!(grant.subject, Subject::Agent(_)));
        workspace.policy.grants.extend(agents);
    }
    if held_authored != authored {
        workspace
            .policy
            .grants
            .retain(|grant| !is_authorship(grant));
        workspace.policy.grants.extend(authored);
    }
    true
}

/// The grants `grants` that `pick` names, in a settled order so two readings of the same policy
/// compare equal.
fn picked_grants<'a>(
    grants: impl IntoIterator<Item = &'a Grant>,
    pick: impl Fn(&Grant) -> bool,
) -> Vec<Grant> {
    let mut grants: Vec<Grant> = grants
        .into_iter()
        .filter(|grant| pick(grant))
        .cloned()
        .collect();
    grants.sort_by(|a, b| a.id.cmp(&b.id));
    grants
}

/// Whether `grant` makes an application an author of its own records in the
/// workspace, as [`tilo_action_policy::roles::app_authorship`] gives it: the
/// workspace's governance gives these, unlike the session's own app grants,
/// and the desk takes them from the governance alone.
fn is_authorship(grant: &Grant) -> bool {
    grant.id.as_str().starts_with("app-author:") && matches!(grant.subject, Subject::App(_))
}

/// Whether `grant` is the one making `app` an author of its own records in the workspace.
fn is_authorship_of(grant: &Grant, app: &str) -> bool {
    is_authorship(grant) && grant.id.as_str() == format!("app-author:{app}")
}

/// Write `edit` into the workspace `id` names on the desk, asking nothing of a desk that does not
/// list it.
fn edit_workspace(
    desk: &dyn Workspaces,
    id: WorkspaceId,
    mut edit: impl FnMut(&mut Workspace) -> Result<bool, String>,
) -> Result<(), String> {
    desk.edit(
        &mut |workspaces| match workspaces.iter_mut().find(|shown| shown.id() == id) {
            Some(shown) => edit(shown),
            None => Ok(false),
        },
    )
}
