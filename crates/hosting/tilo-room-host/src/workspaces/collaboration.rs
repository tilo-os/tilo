//! Invitations, their redemption and member administration of a governed
//! workspace on this relay.
//!
//! What a governor records between the steps of an exchange is the shared
//! [`JoinLedger`], the same on every host that governs workspaces; this module
//! owns who may ask what of this relay's workspaces, and the durability barrier
//! before each answer.

use ed25519_dalek::SigningKey;
use serde::{Deserialize, Serialize};
use tilo_peer_state::join::{JoinBootstrap, JoinProof, VerifiedRedemption};
use tilo_peer_state::join_answer::{JoinWorkspaces, WorkspaceTxn};
use tilo_peer_state::join_ledger::{Administered, Governed, Signer};
pub use tilo_peer_state::join_ledger::{JoinError as CollaborationError, Redeeming};
use tilo_relay_session::workspaces::{
    CancelInvitationRequest, IssueInvitationRequest, IssuedInvitation, PendingInvitation,
    RemoveMemberRequest, SetMemberRoleRequest,
};
use tilo_user_identity::DeviceCert;

use tilo_action_policy::roles::governance;

use super::authority::WorkspaceOp;
use super::{GovernedWorkspace, WorkspaceRecord, Workspaces, system_time};
use crate::host_authority::HostOperation;
use tilo_lock::MutexRecover as _;

pub(super) use tilo_peer_state::join_ledger::JoinLedger as CollaborationState;

/// A change to who is in a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum Membership {
    Remove,
    Ban,
    Unban,
    /// No changes until this Unix millisecond; reading stays allowed.
    Restrict(u64),
    Unrestrict,
}

/// A committed governance change.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct GovernanceCompletion {
    /// The workspace generation that carries it.
    pub generation: u64,
}

/// One waiting for its workspace's next write: whether it staged a change of
/// the workspace's pre-admissions as well as of its record, and where the
/// outcome goes.
pub(super) struct Written {
    admissions: bool,
    answer: tokio::sync::oneshot::Sender<Result<(), CollaborationError>>,
}

impl GovernedWorkspace {
    /// The workspace as its join ledger reads it: committed now.
    pub(super) fn governed(&self) -> Governed<'_> {
        governed(&self.record, self.peer().into())
    }

    /// Keep the people `removed` out of the workspace's public door for the
    /// day: moderation took their membership away. `by` is the person who
    /// asked, when a person asks: someone who leaves is not kept out of a
    /// workspace they may join again.
    pub(super) fn keep_out_removed(
        &mut self,
        removed: &std::collections::BTreeSet<[u8; 32]>,
        by: Option<[u8; 32]>,
        now_ns: u64,
    ) -> Result<(), String> {
        for person in removed {
            if Some(*person) != by {
                self.door.remove(*person, now_ns)?;
            }
        }
        Ok(())
    }
}

/// `record` and `peer`, a workspace's committed state, as its join ledger reads it.
fn governed<'a>(
    record: &'a WorkspaceRecord,
    peer: tilo_peer_state::join_ledger::PeerRef<'a>,
) -> Governed<'a> {
    Governed {
        peer,
        host: &record.authority,
        workspace_id: record.workspace_id,
        generation: record.generation,
        // A newcomer admits the workspace's governance alone: one message
        // holding its complete committed document. What the record retains of
        // its own publications builds on that document, so it reaches the
        // newcomer through the workspace's sync, not through this answer.
        governance_messages: None,
    }
}

fn random<const N: usize>() -> Result<[u8; N], CollaborationError> {
    let mut bytes = [0; N];
    getrandom::fill(&mut bytes).map_err(|error| CollaborationError::new(error.to_string()))?;
    Ok(bytes)
}

impl Workspaces {
    /// Issue an invitation to `workspace` for `administrator`: a device of this
    /// relay's own user, or of a member the workspace lets invite with the
    /// invitation's role.
    pub async fn issue(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let issued = {
            let governed = self.workspace(workspace).await?;
            let mut governed = governed.lock().await;
            self.authorize(
                administrator,
                Some(&governed),
                WorkspaceOp::Invite(request.role.into()),
                now,
            )?;
            self.stage_issue(&mut governed, administrator, request, now)?
        };
        self.written(workspace, false).await?;
        Ok(issued)
    }

    /// Record an invitation `request` asks of `workspace`, unwritten.
    pub(super) fn stage_issue(
        &self,
        workspace: &mut GovernedWorkspace,
        administrator: &DeviceCert,
        request: IssueInvitationRequest,
        now: u64,
    ) -> Result<IssuedInvitation, CollaborationError> {
        let role = request.role;
        let transport = vec![self.transport_hint(workspace)?];
        let mut ledger = workspace.collaboration().clone();
        let issued = ledger.issue(
            &workspace.governed(),
            administrator,
            request,
            transport,
            Signer {
                key: &self.inner.host.key,
                nonce: random()?,
            },
            now,
        )?;
        if ledger != *workspace.collaboration() {
            workspace.record.collaboration = ledger;
            tracing::info!(
                workspace = %workspace.workspace(),
                invite_id = %hex::encode(issued.invite_id),
                ?role,
                outcome = "issued",
                "workspace invitation issued"
            );
        }
        Ok(issued)
    }

    /// Cancel an unredeemed invitation of `workspace`.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    pub async fn cancel(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: CancelInvitationRequest,
    ) -> Result<(), CollaborationError> {
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::INVITE),
            tilo_clock::unix_now_ns(),
        )?;
        let mut state = workspace.collaboration().clone();
        if state.cancel(&request)? {
            workspace.commit_state(state)?;
        }
        Ok(())
    }

    /// The invitations of `workspace` still redeemable.
    pub async fn pending(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
    ) -> Result<Vec<PendingInvitation>, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let workspace = workspace.lock().await;
        self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Govern(governance::INVITE),
            now,
        )?;
        Ok(workspace.collaboration().pending(now))
    }

    /// Admit `device`, a device of this relay's own user, to `workspace` as its
    /// owner. Returns whether governance changed.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its decision is committed, published and answered"
    )]
    pub async fn enter(
        &self,
        workspace: &str,
        device: &DeviceCert,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            device,
            None,
            WorkspaceOp::Host(HostOperation::EnterWorkspace),
            now,
        )?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let mut candidate = workspace.peer().clone();
        if !candidate.admit_certificate(device.clone(), &self.inner.host.key, system_time(now))? {
            return Ok(false);
        }
        let state = workspace.collaboration().clone();
        workspace.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
            Ok(state)
        })?;
        self.settle(&mut workspace).await?;
        Ok(true)
    }

    /// Change the role of the member `request.user_public_key` in `workspace`.
    pub async fn set_member_role(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: SetMemberRoleRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize_role_change(&workspace, administrator, &request, now)?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request.request_id,
            request_digest(&request)?,
            |peer| {
                peer.governance_mut().set_member_role(
                    request.user_public_key,
                    request.role,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Write workspace grants the governor decided (workspace settings such as a
    /// role's Deny on a kind), keyed by their ids.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators and grants for another workspace.
    pub async fn put_room_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        grants: Vec<tilo_action_policy::Grant>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(HostOperation::PutWorkspaceGrants),
            now,
        )?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&grants)?,
            |peer| {
                peer.governance_mut()
                    .put_governed_grants(&grants, &key, system_time(now))
            },
            now,
        )
        .await
    }

    /// Take back the workspace grants `ids`, as the workspace's host decided; a grant
    /// the workspace does not hold is no change.
    ///
    /// # Errors
    ///
    /// Why the grants were not taken back.
    pub async fn remove_room_grants(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        ids: Vec<tilo_action_policy::GrantId>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(HostOperation::PutWorkspaceGrants),
            now,
        )?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("room.grants.remove", &ids))?,
            |peer| {
                peer.governance_mut()
                    .remove_governed_grants(&ids, &key, system_time(now))
            },
            now,
        )
        .await
    }

    /// Make the workspace's app authorship what its host configured: every app
    /// in `apps` authors the workspace's content
    /// ([`tilo_action_policy::roles::app_authorship`]), so a member whose
    /// role lets them change the workspace can do so through it, and every app in
    /// `withdrawn` (one the host made an author before and names no longer)
    /// stops. A workspace already that way is left as it is, so the host settles
    /// this at every start without a governance change each time. Whether a
    /// member may write stays their role's: a viewer still writes nothing.
    ///
    /// Answers whether anything changed.
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, invalid application ids and changes that
    /// could not commit.
    pub async fn settle_app_authorship(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        apps: &std::collections::BTreeSet<String>,
        withdrawn: &std::collections::BTreeSet<String>,
    ) -> Result<bool, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(HostOperation::PutWorkspaceGrants),
            now,
        )?;
        for app in apps.iter().chain(withdrawn) {
            tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
                app: app.clone(),
            }
            .validate()?;
        }
        let state = self.workspace(workspace).await?;
        let mut governed = state.lock().await;
        let granted_by =
            tilo_action_policy::Subject::Person(hex::encode(administrator.user_public_key));
        let held = governed
            .peer()
            .permissions(administrator.device_public_key, system_time(now))
            .map_err(|error| error.to_string())?
            .policy
            .grants;
        let grants: Vec<tilo_action_policy::Grant> = apps
            .iter()
            .map(|app| {
                tilo_action_policy::roles::app_authorship(workspace, app, granted_by.clone())
            })
            .filter(|grant| !held.contains(grant))
            .collect();
        let revoked: Vec<tilo_action_policy::GrantId> = withdrawn
            .iter()
            .filter(|app| !apps.contains(*app))
            .map(|app| {
                tilo_action_policy::roles::app_authorship(workspace, app, granted_by.clone()).id
            })
            .filter(|id| held.iter().any(|grant| &grant.id == id))
            .collect();
        if grants.is_empty() && revoked.is_empty() {
            return Ok(false);
        }
        let digest = request_digest(&("app.authorship.settle", &grants, &revoked))?;
        // One request per change from this generation: settling the same
        // configuration again later finds nothing to change and asks nothing.
        let request_id = request_digest(&(digest, governed.record.generation))?;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut governed,
            request_id,
            digest,
            |peer| {
                let added = !grants.is_empty()
                    && peer.governance_mut().put_governed_grants(
                        &grants,
                        &key,
                        system_time(now),
                    )?;
                let removed = !revoked.is_empty()
                    && peer.governance_mut().remove_governed_grants(
                        &revoked,
                        &key,
                        system_time(now),
                    )?;
                Ok(added || removed)
            },
            now,
        )
        .await?;
        drop(governed);
        tracing::info!(
            %workspace,
            authors = ?apps,
            withdrawn = revoked.len(),
            "room app authorship settled"
        );
        Ok(true)
    }

    /// Make `user_public_key` an administrator of `workspace`: an editor holding
    /// every governance action, still bounded by their role
    /// ([`tilo_action_policy::roles::administrator_grant`]).
    ///
    /// # Errors
    ///
    /// Refuses non-administrators, the structural owner and people the
    /// workspace never admitted.
    pub async fn set_member_administrator(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        user_public_key: [u8; 32],
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(HostOperation::SetAdministrator),
            now,
        )?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&("administrator", user_public_key))?,
            |peer| {
                peer.governance_mut().set_member_administrator(
                    user_public_key,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Remove the member `request.user_public_key` and every device of theirs
    /// from `workspace`. The relay stops admitting them before this returns.
    pub async fn remove_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Remove)
            .await
    }

    /// Remove the members `people` from `workspace` in one governance change and
    /// forget how they joined. A public workspace prunes guests who left this
    /// way: every join costs time in proportion to the workspace's members, so a
    /// workspace that kept every visitor would slow with each one, and one change
    /// per guest would hold the workspace as long again. A pruned guest joins
    /// again through a new invitation.
    pub async fn prune_members(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request_id: [u8; 32],
        people: &std::collections::BTreeSet<[u8; 32]>,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        for person in people {
            let change = tilo_action_policy::Change::RemoveMember {
                person: hex::encode(person),
            };
            self.authorize(
                administrator,
                Some(&workspace),
                WorkspaceOp::Change(&change),
                now,
            )?;
        }
        let key = self.inner.host.key.clone();
        let completion = self
            .administer(
                &mut workspace,
                request_id,
                request_digest(people)?,
                |peer| {
                    let mut changed = false;
                    for person in people {
                        changed |=
                            peer.governance_mut()
                                .remove_member(*person, &key, system_time(now))?;
                    }
                    Ok(changed)
                },
                now,
            )
            .await?;
        let mut state = workspace.collaboration().clone();
        state.forget(people);
        workspace.commit_state(state)?;
        for person in people {
            workspace
                .door
                .clear(*person)
                .map_err(CollaborationError::new)?;
        }
        tracing::info!(
            workspace = %workspace.workspace(),
            people = people.len(),
            "idle members pruned"
        );
        Ok(completion)
    }

    /// Remove `request.user_public_key` from `workspace`, when a member, and keep
    /// them out until [`Self::unban_member`]: a removal plus their
    /// `block:<person>` Deny.
    pub async fn ban_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Ban)
            .await
    }

    /// Let the banned `request.user_public_key` be admitted to `workspace` again.
    pub async fn unban_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Unban)
            .await
    }

    /// Stop `request.user_public_key` changing anything in `workspace` until
    /// `until_ms` (Unix milliseconds): the workspace's expiring
    /// `restrict:<person>` Deny, which lapses by itself and leaves reading
    /// alone. A newer restriction replaces an older one.
    pub async fn restrict_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        until_ms: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(
            workspace,
            administrator,
            request,
            Membership::Restrict(until_ms),
        )
        .await
    }

    /// Lift `request.user_public_key`'s restriction in `workspace` before it
    /// lapses.
    pub async fn unrestrict_member(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        self.change_membership(workspace, administrator, request, Membership::Unrestrict)
            .await
    }

    /// Remove, ban or unban, decided by who asks: a device of the relay's
    /// user governs its workspaces, and any other member by the workspace's policy (a
    /// Moderator reaches members ranked below Moderator).
    async fn change_membership(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        request: RemoveMemberRequest,
        membership: Membership,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let person = hex::encode(request.user_public_key);
        let change = match membership {
            Membership::Remove => tilo_action_policy::Change::RemoveMember {
                person: person.clone(),
            },
            Membership::Ban => tilo_action_policy::Change::Ban {
                person: person.clone(),
            },
            Membership::Unban => tilo_action_policy::Change::Unban {
                person: person.clone(),
            },
            Membership::Restrict(until_ms) => tilo_action_policy::Change::Restrict {
                person: person.clone(),
                until_ms,
            },
            Membership::Unrestrict => tilo_action_policy::Change::Unrestrict {
                person: person.clone(),
            },
        };
        let identity = workspace.workspace().to_owned();
        let by = self.authorize(
            administrator,
            Some(&workspace),
            WorkspaceOp::Change(&change),
            now,
        )?;
        let key = self.inner.host.key.clone();
        let target = request.user_public_key;
        let completion = self
            .administer(
                &mut workspace,
                request.request_id,
                request_digest(&(membership, &request))?,
                |peer| match membership {
                    Membership::Remove => {
                        peer.governance_mut()
                            .remove_member(target, &key, system_time(now))
                    }
                    Membership::Ban => {
                        peer.governance_mut()
                            .ban_member(target, &by, &key, system_time(now))
                    }
                    Membership::Unban => {
                        peer.governance_mut()
                            .unban_member(target, &key, system_time(now))
                    }
                    Membership::Restrict(until_ms) => peer.governance_mut().put_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant(
                            &identity,
                            &person,
                            until_ms,
                            tilo_action_policy::Subject::Person(by.clone()),
                        )],
                        &key,
                        system_time(now),
                    ),
                    Membership::Unrestrict => peer.governance_mut().remove_governed_grants(
                        &[tilo_action_policy::roles::restriction_grant_id(&person)],
                        &key,
                        system_time(now),
                    ),
                },
                now,
            )
            .await?;
        if matches!(membership, Membership::Remove | Membership::Ban) {
            // A removal takes back every way in the workspace handed them, not
            // only their seat: an invitation issued to them before, still
            // unredeemed, would otherwise admit them again at once.
            let mut state = workspace.collaboration().clone();
            state.withdraw_invitations(&std::collections::BTreeSet::from([target]));
            workspace.commit_state(state)?;
            workspace
                .keep_out_removed(
                    &std::collections::BTreeSet::from([target]),
                    Some(administrator.user_public_key),
                    now,
                )
                .map_err(CollaborationError::new)?;
        }
        if matches!(membership, Membership::Unban) {
            workspace
                .door
                .clear(target)
                .map_err(CollaborationError::new)?;
        }
        tracing::info!(
            workspace = %workspace.workspace(),
            %person,
            outcome = ?membership,
            "workspace membership change committed"
        );
        Ok(completion)
    }

    /// Commit `command`, a membership change `device` makes itself, in
    /// `workspace` on its behalf, as the workspace's governor: for the device whose
    /// own host this is. That device and this host sign with one key, so
    /// the workspace's history has one writer under it, this host, and the
    /// device's own replica only ever receives what is written here. The
    /// workspace's policy decides the change against `device`'s grants, as it
    /// decides any member's.
    ///
    /// Answers the workspace's generation that carries the change, and the
    /// changes it added to the workspace's document, for the device's replica
    /// to take in at once (none when the workspace carried it already).
    ///
    /// # Errors
    /// Refuses a device other than this host's own, a change the workspace's
    /// rules do not let `device` make, and a change that could not commit.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the workspace stays locked until its change is committed and published"
    )]
    pub async fn commit_own_membership(
        &self,
        workspace: &str,
        device: &DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<(GovernanceCompletion, Vec<Vec<u8>>), CollaborationError> {
        if device.device_public_key != self.inner.host.cert.device_public_key {
            return Err(CollaborationError::new(
                "a membership change is committed here only for this host's own device",
            ));
        }
        command.validate()?;
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        let mut changed = workspace.peer().clone();
        if !changed.workspace_mut().apply_workspace_membership_command(
            device,
            &command,
            &key,
            system_time(now),
        )? {
            return Ok((
                GovernanceCompletion {
                    generation: workspace.record.generation,
                },
                Vec::new(),
            ));
        }
        let before = workspace.peer().workspace_heads().unwrap_or_default();
        // A member removed takes every way in the workspace handed them with
        // them, as when a member's own device sends the removal.
        let removed: std::collections::BTreeSet<_> =
            super::member_roots(&workspace.peer(), system_time(now))
                .difference(&super::member_roots(&changed, system_time(now)))
                .copied()
                .collect();
        let generation = workspace.commit(changed, &key, system_time(now), |record, _| {
            let mut state = record.collaboration.clone();
            state.withdraw_invitations(&removed);
            Ok(state)
        })?;
        workspace
            .keep_out_removed(
                &removed,
                Some(device.user_public_key),
                tilo_clock::unix_now_ns(),
            )
            .map_err(CollaborationError::new)?;
        self.settle(&mut workspace).await?;
        let added = workspace.peer().workspace_changes_since(&before);
        Ok((GovernanceCompletion { generation }, added))
    }

    /// Install a bot as a governed member of `workspace`, update its program,
    /// renew its delegation or uninstall it
    /// ([`WorkspaceMembershipCommand::InstallBot`],
    /// [`WorkspaceMembershipCommand::RenewBot`], `UninstallBot`), as the
    /// workspace's administrator. Every member reads and enforces the result, so it is
    /// a governed change rather than this host's own state.
    ///
    /// # Errors
    /// Refuses non-administrators and anything else; the peer refuses an
    /// install or renewal its rules do not admit.
    ///
    /// [`WorkspaceMembershipCommand::InstallBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallBot
    /// [`WorkspaceMembershipCommand::RenewBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RenewBot
    pub async fn govern_bot(
        &self,
        workspace: &str,
        administrator: &DeviceCert,
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(
            command,
            Command::InstallBot(_) | Command::RenewBot(_) | Command::UninstallBot { .. }
        ) {
            return Err(CollaborationError::new(
                "only installing, renewing or uninstalling a bot governs a bot",
            ));
        }
        let now = tilo_clock::unix_now_ns();
        self.authorize(
            administrator,
            None,
            WorkspaceOp::Host(HostOperation::GovernBot),
            now,
        )?;
        command.validate()?;
        // The command names itself: the same install or renewal sent again
        // is the same administration request.
        let digest = request_digest(&("bot.govern", &command))?;
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        let key = self.inner.host.key.clone();
        workspace
            .peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(administrator, &command, &key, system_time(now))?;
        self.administer(
            &mut workspace,
            digest,
            digest,
            |peer| {
                peer.workspace_mut().apply_workspace_membership_command(
                    administrator,
                    &command,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// Ask `workspace`'s host to run a bot from the directory, or take the
    /// request back ([`WorkspaceMembershipCommand::RequestBot`],
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]), as a member the
    /// workspace lets install (or uninstall) bots: any such member of a workspace this
    /// host governs, not only its administrators. The request is a governed
    /// change every member reads; [`Self::bot_requests`] is what the host
    /// follows.
    ///
    /// # Errors
    /// Refuses another command, a malformed request, and a member the workspace
    /// does not let install (or uninstall) bots.
    ///
    /// [`WorkspaceMembershipCommand::RequestBot`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::RequestBot
    /// [`WorkspaceMembershipCommand::WithdrawBotRequest`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::WithdrawBotRequest
    pub async fn ask_for_bot(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        let gate = match &command {
            Command::RequestBot(_) => governance::INSTALL_BOT,
            Command::WithdrawBotRequest { .. } => governance::UNINSTALL_BOT,
            _ => {
                return Err(CollaborationError::new(
                    "only asking for a bot or taking the request back asks for a bot",
                ));
            }
        };
        self.govern_as_member(workspace, member, request_id, command, gate, "bot.request")
            .await
    }

    /// Choose the version of an app `workspace` runs for every member, or take
    /// the choice back ([`WorkspaceMembershipCommand::InstallApp`],
    /// [`WorkspaceMembershipCommand::UninstallApp`]), as a member the workspace
    /// lets install apps. The choice is a governed change every member
    /// reads ([`Self::app_versions`]); each is to run it, and a
    /// change made with another version waits until its author has.
    ///
    /// # Errors
    /// Refuses another command, a malformed release, and a member the workspace
    /// does not let install apps.
    ///
    /// [`WorkspaceMembershipCommand::InstallApp`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::InstallApp
    /// [`WorkspaceMembershipCommand::UninstallApp`]: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::UninstallApp
    pub async fn govern_app(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        if !matches!(
            command,
            Command::InstallApp(_) | Command::UninstallApp { .. }
        ) {
            return Err(CollaborationError::new(
                "only installing or uninstalling an app version governs an app",
            ));
        }
        self.govern_as_member(
            workspace,
            member,
            request_id,
            command,
            governance::INSTALL_APP,
            "app.version",
        )
        .await
    }

    /// The app versions `workspace` runs, as its governance records them now.
    pub async fn app_versions(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::InstalledApp>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .app_versions())
    }

    /// Apply `command` to `workspace` as `member`, whom the workspace must allow the
    /// governance action `gate`, and publish it; `tag` names the request
    /// kind in its digest.
    async fn govern_as_member(
        &self,
        workspace: &str,
        member: &DeviceCert,
        request_id: [u8; 32],
        command: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        gate: &'static str,
        tag: &'static str,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        command.validate()?;
        let now = tilo_clock::unix_now_ns();
        let workspace = self.workspace(workspace).await?;
        let mut workspace = workspace.lock().await;
        self.authorize(member, Some(&workspace), WorkspaceOp::Govern(gate), now)?;
        let key = self.inner.host.key.clone();
        workspace
            .peer()
            .clone()
            .workspace_mut()
            .apply_workspace_membership_command(member, &command, &key, system_time(now))?;
        self.administer(
            &mut workspace,
            request_id,
            request_digest(&(tag, &command))?,
            |peer| {
                peer.workspace_mut().apply_workspace_membership_command(
                    member,
                    &command,
                    &key,
                    system_time(now),
                )
            },
            now,
        )
        .await
    }

    /// The bots `workspace` asks its host to run, as its governance records
    /// them now.
    pub async fn bot_requests(
        &self,
        workspace: &str,
    ) -> Result<Vec<tilo_peer_state::RequestedBot>, CollaborationError> {
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .bot_requests())
    }

    /// Installation `id` of `workspace` as its governance records it now: the
    /// live row, else the last withdrawn one.
    pub async fn installed_bot(
        &self,
        workspace: &str,
        id: &str,
    ) -> Result<Option<tilo_peer_state::InstalledBot>, CollaborationError> {
        let now = system_time(tilo_clock::unix_now_ns());
        Ok(self
            .workspace(workspace)
            .await?
            .lock()
            .await
            .peer()
            .governance()
            .installation(id, now))
    }

    pub(super) async fn administer(
        &self,
        workspace: &mut GovernedWorkspace,
        request_id: [u8; 32],
        digest: [u8; 32],
        change: impl FnOnce(&mut tilo_peer_state::StatePeer) -> Result<bool, String>,
        now: u64,
    ) -> Result<GovernanceCompletion, CollaborationError> {
        if let Administered::Done(generation) =
            workspace.collaboration().administered(request_id, digest)?
        {
            return Ok(GovernanceCompletion { generation });
        }
        let mut candidate = workspace.peer().clone();
        change(&mut candidate)?;
        let mut state = workspace.collaboration().clone();
        state.record_administration(
            request_id,
            digest,
            workspace.record.generation.saturating_add(1),
        );
        let generation =
            workspace.commit(candidate, &self.inner.host.key, system_time(now), |_, _| {
                Ok(state)
            })?;
        self.settle(workspace).await?;
        Ok(GovernanceCompletion { generation })
    }

    /// Wait until `workspace`'s next write, which takes the workspace once for every
    /// change staged in it before: a crowd joining wrote the whole workspace once
    /// per challenge, one after another under its lock.
    /// With `admissions`, the write saves the workspace's pre-admissions too.
    pub(super) async fn written(
        &self,
        workspace: &str,
        admissions: bool,
    ) -> Result<(), CollaborationError> {
        let (answer, wait) = tokio::sync::oneshot::channel();
        if self.queue_write(workspace, Written { admissions, answer }) {
            // Its own task: a request that goes away mid-write must not cut
            // off the answers of everyone else waiting on it.
            let workspaces = self.clone();
            let workspace = workspace.to_owned();
            tokio::spawn(async move { workspaces.write_queued(&workspace).await });
        }
        wait.await
            .map_err(|_| CollaborationError::new("the room stopped writing"))?
    }

    /// Queue `written` for `workspace`'s next write. True when it is the first
    /// since the workspace last took them, so its sender starts the write.
    fn queue_write(&self, workspace: &str, written: Written) -> bool {
        let mut queued = self.inner.queued_writes.lock_recover();
        let waiting = queued.entry(workspace.to_owned()).or_default();
        waiting.push(written);
        let first = waiting.len() == 1;
        drop(queued);
        first
    }

    async fn write_queued(&self, identity: &str) {
        let take = || {
            self.inner
                .queued_writes
                .lock_recover()
                .remove(identity)
                .unwrap_or_default()
        };
        let (result, waiting) = match self.workspace(identity).await {
            Err(error) => (Err(error), take()),
            Ok(workspace) => {
                let mut workspace = workspace.lock().await;
                // Taken under the workspace: whoever queued staged their change
                // before.
                let waiting = take();
                let mut result = workspace.persist();
                if result.is_ok() && waiting.iter().any(|written| written.admissions) {
                    result = workspace.admissions.save();
                }
                drop(workspace);
                (result.map_err(CollaborationError::new), waiting)
            }
        };
        for written in waiting {
            let _ = written.answer.send(result.clone());
        }
    }

    /// Verify `proof` against the challenge `workspace` issued to the device at
    /// `remote_endpoint`, for an invitation still redeemable.
    pub(super) fn redeem(
        workspace: &GovernedWorkspace,
        remote_endpoint: [u8; 32],
        proof: &JoinProof,
        now: u64,
    ) -> Result<VerifiedRedemption, CollaborationError> {
        workspace
            .collaboration()
            .redeem(&workspace.governed(), remote_endpoint, proof, now)
    }

    /// Admit the verified `joins` to `workspace` in one governance change: the
    /// shared admission ([`tilo_peer_state::join_answer::admit`]), published before anyone
    /// hears their answer. People arriving together cost the workspace one write
    /// and one publication instead of one each, and each of those grows
    /// with the workspace.
    ///
    /// Answers each join in order. Fails as a whole when the change cannot
    /// be committed or published.
    pub(super) async fn admit_redemptions(
        &self,
        workspace: &mut GovernedWorkspace,
        joins: &[Redeeming<'_>],
        now: u64,
    ) -> Result<Vec<Result<JoinBootstrap, CollaborationError>>, CollaborationError> {
        let key = &self.inner.host.key;
        let mut held = HeldWorkspace::new(key, workspace);
        let admitted = tilo_peer_state::join_answer::admit(&mut held, joins, key, now);
        if held.committed {
            self.settle(workspace).await?;
            // Someone invited from the wait list is in now: their entry
            // leaves it.
            workspace.forget_joined_wait_list();
            // The person whose own workspace this is administers it from
            // their first device on.
            let owner = self
                .own()
                .and_then(|own| own.owner_of(workspace.workspace()))
                .filter(|owner| {
                    joins
                        .iter()
                        .any(|join| join.redemption.certificate().user_public_key == *owner)
                });
            if let Some(owner) = owner {
                self.make_administrator(workspace, owner, now).await?;
            }
        }
        admitted
    }

    /// Make `person`, admitted to the workspace of their own `workspace`, its
    /// administrator; nothing while they have no device there.
    pub(super) async fn administer_own(
        &self,
        workspace: &str,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let held = self.workspace(workspace).await?;
        let mut held = held.lock().await;
        let members = held
            .peer()
            .governance()
            .governed_members(system_time(now))
            .map_err(CollaborationError::new)?;
        if members
            .iter()
            .any(|member| member.user_public_key == person)
        {
            self.make_administrator(&mut held, person, now).await?;
        }
        drop(held);
        Ok(())
    }

    /// Make the admitted `person` an administrator of `workspace`, held: an
    /// Editor with every governance and moderation action. Writes nothing
    /// when they are one.
    async fn make_administrator(
        &self,
        workspace: &mut GovernedWorkspace,
        person: [u8; 32],
        now: u64,
    ) -> Result<(), CollaborationError> {
        let key = &self.inner.host.key;
        let mut candidate = workspace.peer().clone();
        if !candidate
            .governance_mut()
            .set_member_administrator(person, key, system_time(now))?
        {
            return Ok(());
        }
        let state = workspace.collaboration().clone();
        workspace.commit(candidate, key, system_time(now), |_, _| Ok(state))?;
        self.settle(workspace).await?;
        Ok(())
    }
}

/// A workspace of this relay while a governor decides, under its lock: whether
/// its ledger changed unwritten, and whether a governance change was
/// committed that its members have not been sent yet.
struct HeldWorkspace<'a> {
    key: &'a SigningKey,
    workspace: &'a mut GovernedWorkspace,
    staged: bool,
    committed: bool,
}

impl<'a> HeldWorkspace<'a> {
    const fn new(key: &'a SigningKey, workspace: &'a mut GovernedWorkspace) -> Self {
        Self {
            key,
            workspace,
            staged: false,
            committed: false,
        }
    }
}

impl WorkspaceTxn for HeldWorkspace<'_> {
    fn workspace(&self) -> &str {
        self.workspace.workspace()
    }

    fn governed(&self) -> Governed<'_> {
        self.workspace.governed()
    }

    fn ledger(&self) -> &CollaborationState {
        self.workspace.collaboration()
    }

    fn stage_ledger(&mut self, ledger: CollaborationState) {
        if *self.workspace.collaboration() != ledger {
            self.workspace.record.collaboration = ledger;
            self.staged = true;
        }
    }

    fn commit_governance(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.workspace
            .commit(peer, self.key, system_time(now), |record, peer| {
                ledger(&governed(record, peer.into())).map_err(|error| error.to_string())
            })?;
        // The commit wrote the whole record, the ledger staged before
        // included.
        self.staged = false;
        self.committed = true;
        Ok(())
    }

    fn commit_admission(
        &mut self,
        peer: tilo_peer_state::StatePeer,
        now: u64,
        ledger: &mut dyn FnMut(&Governed<'_>) -> Result<CollaborationState, CollaborationError>,
    ) -> Result<(), CollaborationError> {
        self.workspace
            .commit_admission(peer, self.key, system_time(now), |record, peer| {
                ledger(&governed(record, peer.into())).map_err(|error| error.to_string())
            })?;
        self.staged = false;
        self.committed = true;
        Ok(())
    }
}

/// This relay's workspaces, to the governor that answers their workspace-join
/// exchange: a change is staged in the workspace's record and written once for
/// everyone who staged one meanwhile (`Workspaces::written`); a governance
/// change is committed under the workspace's lock and published before the
/// lock is released.
impl JoinWorkspaces for Workspaces {
    fn key(&self) -> &SigningKey {
        &self.inner.host.key
    }

    fn nonce(&self) -> Result<[u8; 32], CollaborationError> {
        random()
    }

    async fn with_workspace<T: Send>(
        &self,
        workspace: &str,
        decide: impl FnOnce(&mut dyn WorkspaceTxn) -> Result<T, CollaborationError> + Send,
    ) -> Result<T, CollaborationError> {
        let held = self.workspace(workspace).await?;
        let mut locked = held.lock().await;
        let (answer, staged, committed) = {
            let mut held = HeldWorkspace::new(&self.inner.host.key, &mut locked);
            let answer = decide(&mut held);
            (answer, held.staged, held.committed)
        };
        if committed {
            self.settle(&mut locked).await?;
        }
        drop(locked);
        if staged {
            self.written(workspace, false).await?;
        }
        answer
    }
}

impl GovernedWorkspace {
    /// Whether a device redeemed the invitation `invite_id`.
    pub(super) fn redeemed(&self, invite_id: [u8; 32]) -> bool {
        self.collaboration().redeemed(invite_id)
    }

    /// The certified device admitted by the committed request receipt.
    pub(super) fn admitted_device(&self, request_id: [u8; 32]) -> Option<[u8; 32]> {
        self.collaboration().admitted_device(request_id)
    }
}

pub(super) use tilo_peer_state::join_ledger::request_digest;
