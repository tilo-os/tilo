//! Grants recorded in a workspace, and the effect receipts that note them in
//! its history.
//!
//! Pure mutations of the tree: every host that governs a room records a
//! grant and its receipt the same way, then mirrors the change into its room
//! document and persists it however it keeps state.

use tilo_action_policy::{Change, Grant, Refusal};
use tilo_actions::{ActionContext, ActionKey, Described, Did, Scalar};

use crate::ids::WorkspaceId;
use crate::task::WorkspaceEffectReceipt;
use crate::tree::DesktopTree;
use crate::workspace::{UserId, WORKSPACE_GOVERNANCE_APP_ID};

impl DesktopTree {
    /// Grant `grant` in `workspace_id` on behalf of `grantor` at `now`: the
    /// grant as stored, or why it was refused (the workspace is gone, the
    /// grantor may not grant, or not this much). Whether this machine may
    /// write the room's governance at all is the host's question, asked
    /// first.
    ///
    /// # Errors
    /// The governance refusal.
    pub fn grant_in_workspace_at(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        grant: Grant,
        now: u64,
    ) -> Result<Grant, Refusal> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)
            .ok_or_else(|| Refusal::OutsideRoom(grant.node.clone()))?;
        workspace.govern_as_at(grantor, Change::Grant(grant.clone()), now)?;
        Ok(workspace
            .policy
            .grants
            .iter()
            .find(|held| held.id == grant.id)
            .cloned()
            .unwrap_or(grant))
    }

    /// Record one terminal effect in `workspace_id`'s history, exactly once
    /// per invocation: the receipt as stored, or `None` when the workspace is
    /// gone or already holds a receipt for the invocation. An application's
    /// effect is recorded [`WorkspaceEffectReceipt::redacted`]; a record the
    /// host writes itself, such as a grant a person gave,
    /// [`WorkspaceEffectReceipt::recorded`].
    pub fn append_workspace_effect_receipt(
        &mut self,
        workspace_id: WorkspaceId,
        receipt: WorkspaceEffectReceipt,
    ) -> Option<WorkspaceEffectReceipt> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)?;
        if workspace
            .effect_receipts
            .iter()
            .any(|held| held.invocation == receipt.invocation)
        {
            return None;
        }
        workspace.effect_receipts.push(receipt.clone());
        Some(receipt)
    }
}

/// The verb a workspace's history records when a person allows an app to
/// use another app's action.
pub const ALLOW_APP_ACTION: &str = "allow-app-action";

/// What a workspace's history records when a person allows `app_label` to
/// use `target_label`'s action `action_label` in `room`, for `lifetime`.
#[must_use]
pub fn app_action_grant_context(
    room: &str,
    app_label: &str,
    target_label: &str,
    action_label: &str,
    lifetime: &str,
) -> ActionContext {
    ActionContext::for_resource(
        format!("workspace.permissions:{room}"),
        "workspace.permissions",
        "App actions",
    )
    .with_app(WORKSPACE_GOVERNANCE_APP_ID, "Workspace permissions")
    .with_described(Described::new(Did::verb(
        ActionKey::new(
            WORKSPACE_GOVERNANCE_APP_ID,
            "workspace.permissions",
            ALLOW_APP_ACTION,
        ),
        [
            ("app".to_owned(), Scalar::text(app_label)),
            ("target".to_owned(), Scalar::text(target_label)),
            ("action".to_owned(), Scalar::text(action_label)),
            ("lifetime".to_owned(), Scalar::text(lifetime)),
        ],
    )))
}

#[cfg(test)]
mod tests {
    use tilo_action_policy::{
        Answer, Authority, KindSet, Level, Lifetime, Party, Preset, Rule, Subject, TreePath,
    };

    use super::*;
    use crate::ids::{RoomId, WorkspaceId};
    use crate::workspace::{Workspace, preset_assign_node};
    use tilo_actions::{EffectReceiptStatus, InvocationId};

    fn tree() -> (DesktopTree, WorkspaceId) {
        let id = WorkspaceId(1);
        let mut workspace = Workspace::new(id, "Home");
        workspace.set_room(RoomId::new());
        workspace.set_owner("owner");
        (
            DesktopTree {
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            id,
        )
    }

    /// A grant on one set's assign node lets its holder assign that set and
    /// no other.
    #[test]
    fn a_grant_to_assign_one_role_assigns_that_role_only() {
        let (mut tree, _) = tree();
        let workspace = &mut tree.workspaces[0];
        let room = workspace.policy.room.clone();
        for name in ["Reviewer", "Admin helper"] {
            let owner = workspace.person_authority("owner");
            let preset = Preset::new(
                name,
                "",
                vec![Rule::new(
                    TreePath::room(&room).app("so.tilo.notes"),
                    KindSet::READ,
                    Answer::Allow,
                )],
            );
            let mut next = workspace.clone();
            next.save_preset_with_at(&owner, preset, 0).unwrap();
            *workspace = next;
        }
        workspace.grant(Grant::new(
            "scribe-assigns-reviewer",
            Subject::agent("scribe"),
            preset_assign_node(&room, "Reviewer"),
            KindSet::OUTSIDE,
            Answer::Allow,
            Subject::person("owner"),
        ));
        // A delegation hands out no more than it holds itself.
        workspace.grant(Grant::new(
            "scribe-reads-notes",
            Subject::agent("scribe"),
            TreePath::room(&room).app("so.tilo.notes"),
            KindSet::READ,
            Answer::Allow,
            Subject::person("owner"),
        ));
        let ada = Subject::person("ada");
        let assign = |workspace: &mut Workspace, name: &str| {
            let snapshot = workspace.clone();
            let scribe = Authority::person(snapshot.policy.party("owner")).bounded_by(Party::new(
                vec![Subject::agent("scribe")],
                &snapshot.policy.grants,
            ));
            workspace.assign_preset_with_at(
                "owner",
                &scribe,
                name,
                &ada,
                &Lifetime::UntilRevoked,
                0,
            )
        };
        assert!(
            assign(workspace, "Reviewer").is_ok(),
            "the granted set is assigned"
        );
        assert!(
            assign(workspace, "Admin helper").is_err(),
            "another set is not"
        );
        let reading = TreePath::root()
            .push(Level::Room, room.as_str())
            .app("so.tilo.notes");
        assert_eq!(
            workspace.answer_at("ada", &reading, tilo_action_policy::ActionKind::Read, 0),
            Answer::Allow
        );
    }

    fn app_grant(room: &str) -> Grant {
        Grant::new(
            "app-open",
            Subject::app("so.tilo.caller"),
            TreePath::room(room)
                .app("so.tilo.target")
                .resource("target.document")
                .action("open"),
            KindSet::ALL,
            Answer::Allow,
            Subject::person("someone else"),
        )
    }

    #[test]
    fn only_a_governor_grants_and_the_stored_grant_names_them() {
        let (mut tree, id) = tree();
        let room = tree.workspaces[0].policy.room.clone();
        assert!(
            tree.grant_in_workspace_at(id, &"stranger".to_string(), app_grant(&room), 10)
                .is_err()
        );
        tree.grant_in_workspace_at(id, &"owner".to_string(), app_grant(&room), 10)
            .unwrap();
        let stored = tree.workspaces[0]
            .policy
            .grants
            .iter()
            .find(|grant| grant.id.as_str() == "app-open")
            .unwrap();
        assert_eq!(stored.granted_by, Subject::person("owner"));
        assert!(
            tree.grant_in_workspace_at(WorkspaceId(2), &"owner".to_string(), app_grant(&room), 10)
                .is_err()
        );
    }

    #[test]
    fn a_receipt_is_recorded_once_and_bounded() {
        let (mut tree, id) = tree();
        let invocation = InvocationId::new();
        let context =
            app_action_grant_context("room", "Caller\u{7}", "Target", &"x".repeat(400), "once");
        let receipt = tree
            .append_workspace_effect_receipt(
                id,
                WorkspaceEffectReceipt::recorded(
                    invocation,
                    "owner",
                    context,
                    EffectReceiptStatus::Succeeded,
                    42,
                ),
            )
            .unwrap();
        let Some(Did::Verb(verb)) = &receipt.context.what else {
            panic!("the grant is recorded as a verb");
        };
        assert_eq!(verb.action.action, ALLOW_APP_ACTION);
        let action = verb.args["action"].as_str().unwrap();
        assert!(action.chars().count() <= tilo_actions::entry::MAX_SCALAR_CHARS + 1);
        assert_eq!(
            verb.args["app"].as_str(),
            Some("Caller"),
            "control characters are dropped"
        );
        assert_eq!(
            receipt.context.app.as_ref().unwrap().id,
            WORKSPACE_GOVERNANCE_APP_ID
        );
        assert!(
            receipt
                .context
                .facets
                .iter()
                .any(|facet| facet.key == "tilo.invocation")
        );
        assert!(
            tree.append_workspace_effect_receipt(
                id,
                WorkspaceEffectReceipt::redacted(
                    invocation,
                    "owner",
                    ActionContext::default(),
                    EffectReceiptStatus::Succeeded,
                    43,
                ),
            )
            .is_none()
        );
        assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
    }
}

/// What a governance change wrote into one workspace's policy, so the host
/// mirrors exactly that into the room document. Empty means nothing changed:
/// the actor lacked authority, or there was nothing to do.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PolicyEdit {
    /// People whose membership was set or ended: the mirror writes their
    /// role as it now stands, or removes them.
    pub members: Vec<UserId>,
    pub granted: Vec<tilo_action_policy::GrantId>,
    pub revoked: Vec<tilo_action_policy::GrantId>,
    pub sandbox_granted: Vec<String>,
    pub sandbox_revoked: Vec<String>,
}

impl PolicyEdit {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.members.is_empty()
            && self.granted.is_empty()
            && self.revoked.is_empty()
            && self.sandbox_granted.is_empty()
            && self.sandbox_revoked.is_empty()
    }

    /// The same edit, with `other`'s too.
    #[must_use]
    pub fn and(mut self, other: Self) -> Self {
        self.members.extend(other.members);
        self.granted.extend(other.granted);
        self.revoked.extend(other.revoked);
        self.sandbox_granted.extend(other.sandbox_granted);
        self.sandbox_revoked.extend(other.sandbox_revoked);
        self
    }

    /// Write this edit of `workspace` into the room document `doc`.
    pub fn write(&self, doc: &mut automerge::AutoCommit, workspace: &crate::Workspace) {
        use crate::desktop_doc;
        let id = workspace.id;
        for person in &self.members {
            match workspace.policy.members.get(person) {
                Some(role) => {
                    desktop_doc::set_member(doc, id, person, *role);
                }
                None => {
                    desktop_doc::remove_member(doc, id, person);
                }
            }
        }
        for grant in workspace
            .policy
            .grants
            .iter()
            .filter(|grant| self.granted.contains(&grant.id))
        {
            desktop_doc::insert_grant(doc, id, grant);
        }
        for grant in &self.revoked {
            desktop_doc::revoke_grant(doc, id, grant);
        }
        for cap in workspace
            .sandbox
            .iter()
            .filter(|cap| self.sandbox_granted.contains(&cap.id))
        {
            desktop_doc::insert_sandbox(doc, id, cap);
        }
        for cap in &self.sandbox_revoked {
            desktop_doc::revoke_sandbox(doc, id, cap);
        }
    }
}

impl crate::Workspace {
    /// Apply `changes` as `actor` at `now`, all or none: each is checked
    /// against the policy as the ones before it left it. What changed, for
    /// the host to mirror.
    ///
    /// # Errors
    /// The first change refused; nothing is applied then.
    pub fn govern_all_at(
        &mut self,
        actor: &str,
        changes: Vec<Change>,
        now: u64,
    ) -> Result<PolicyEdit, Refusal> {
        let before = self.policy.clone();
        for change in changes {
            if let Err(refusal) = self.govern_as_at(actor, change, now) {
                self.policy = before;
                return Err(refusal);
            }
        }
        Ok(PolicyEdit::between(&before, &self.policy))
    }

    /// Record a sandbox capability as `actor`, who must be allowed to grant
    /// in the room at `now`.
    ///
    /// # Errors
    /// The actor may not grant.
    pub fn grant_sandbox_as_at(
        &mut self,
        actor: &str,
        cap: collab_types::caps::CapOverride,
        now: u64,
    ) -> Result<PolicyEdit, Refusal> {
        self.require_governance(actor, tilo_action_policy::roles::governance::GRANT, now)?;
        let id = self
            .grant_sandbox(cap.granted_by(collab_types::caps::Principal::Person(actor.to_owned())));
        Ok(PolicyEdit {
            sandbox_granted: vec![id],
            ..PolicyEdit::default()
        })
    }

    /// Drop a sandbox capability as `actor`, who must be allowed to revoke
    /// in the room at `now`.
    ///
    /// # Errors
    /// The actor may not revoke.
    pub fn revoke_sandbox_as_at(
        &mut self,
        actor: &str,
        id: &str,
        now: u64,
    ) -> Result<PolicyEdit, Refusal> {
        self.require_governance(actor, tilo_action_policy::roles::governance::REVOKE, now)?;
        Ok(PolicyEdit {
            sandbox_revoked: self
                .revoke_sandbox(id)
                .then(|| id.to_owned())
                .into_iter()
                .collect(),
            ..PolicyEdit::default()
        })
    }

    fn require_governance(&self, actor: &str, action: &str, now: u64) -> Result<(), Refusal> {
        if self.may_govern_at(actor, action, now) {
            Ok(())
        } else {
            Err(Refusal::NotPermitted(format!(
                "{actor} may not {action} in this room"
            )))
        }
    }
}

impl PolicyEdit {
    /// What changed from `before` to `after`.
    #[must_use]
    pub fn between(
        before: &tilo_action_policy::RoomPolicy,
        after: &tilo_action_policy::RoomPolicy,
    ) -> Self {
        let mut members: Vec<UserId> = before
            .members
            .iter()
            .filter(|(person, role)| after.members.get(*person) != Some(role))
            .map(|(person, _)| person.clone())
            .collect();
        members.extend(
            after
                .members
                .keys()
                .filter(|person| !before.members.contains_key(*person))
                .cloned(),
        );
        let granted = after
            .grants
            .iter()
            .filter(|grant| !before.grants.contains(grant))
            .map(|grant| grant.id.clone())
            .collect();
        let revoked = before
            .grants
            .iter()
            .filter(|grant| after.grants.iter().all(|held| held.id != grant.id))
            .map(|grant| grant.id.clone())
            .collect();
        Self {
            members,
            granted,
            revoked,
            ..Self::default()
        }
    }
}
