//! Grants recorded in a workspace, and the effect receipts that note them in
//! its history.
//!
//! Pure mutations of the tree: every host that governs a room records a
//! grant and its receipt the same way, then mirrors the change into its room
//! document and persists it however it keeps state.

use collab_types::caps::CapOverride;
use tilo_actions::{ActionContext, ActionFacet, EffectReceiptStatus, InvocationId};

use crate::ids::WorkspaceId;
use crate::task::WorkspaceEffectReceipt;
use crate::tree::DesktopTree;
use crate::workspace::{UserId, WORKSPACE_GOVERNANCE_APP_ID};

impl DesktopTree {
    /// Grant `cap` in `workspace_id` on behalf of `grantor` at `now`: the
    /// capability as stored, or `None` when the workspace is gone or the
    /// grantor may not delegate it. Whether this machine may write the
    /// room's governance at all is the host's question, asked first.
    pub fn grant_in_workspace_at(
        &mut self,
        workspace_id: WorkspaceId,
        grantor: &UserId,
        cap: CapOverride,
        now: u64,
    ) -> Option<CapOverride> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)?;
        let id = workspace.grant_as_at(grantor, cap, now)?;
        workspace
            .capabilities
            .iter()
            .find(|capability| capability.id == id)
            .cloned()
    }

    /// Record one terminal effect of `invocation` in `workspace_id`'s
    /// history, exactly once: the receipt as stored, or `None` when the
    /// workspace is gone or already holds a receipt for the invocation.
    /// Every text is bounded and stripped of control characters.
    pub fn append_workspace_effect_receipt_at(
        &mut self,
        workspace_id: WorkspaceId,
        invocation: InvocationId,
        actor: &str,
        mut context: ActionContext,
        status: EffectReceiptStatus,
        completed_at: i64,
    ) -> Option<WorkspaceEffectReceipt> {
        let workspace = self
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.id == workspace_id)?;
        if workspace
            .effect_receipts
            .iter()
            .any(|receipt| receipt.invocation == invocation)
        {
            return None;
        }
        let original_action = bounded_receipt_text(&context.action_label, 160);
        context.action_label = if original_action.is_empty() {
            status.label().to_string()
        } else {
            format!("{} · {original_action}", status.label())
        };
        context
            .facets
            .push(ActionFacet::new("tilo.receipt", "effect"));
        context.facets.push(ActionFacet::new(
            "tilo.invocation",
            invocation.0.to_string(),
        ));
        redact_receipt_context(&mut context);
        let receipt = WorkspaceEffectReceipt {
            invocation,
            actor: bounded_receipt_text(actor, 160),
            context,
            status,
            completed_at,
        };
        workspace.effect_receipts.push(receipt.clone());
        Some(receipt)
    }
}

/// What a workspace's history says when a person allows `app_label` to use
/// `target_label`'s action `action_label` in `room`, for `lifetime`.
#[must_use]
pub fn app_action_grant_context(
    room: &str,
    app_label: &str,
    target_label: &str,
    action_label: &str,
    lifetime: &str,
) -> ActionContext {
    ActionContext::for_resource(
        format!("Allowed {app_label} to use {target_label}: {action_label} {lifetime}"),
        format!("workspace.permissions:{room}"),
        "workspace.permissions",
        "App actions",
    )
    .with_app(WORKSPACE_GOVERNANCE_APP_ID, "Workspace permissions")
}

fn bounded_receipt_text(value: &str, max: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(max)
        .collect::<String>()
        .trim()
        .to_string()
}

fn redact_receipt_context(context: &mut ActionContext) {
    context.action_label = bounded_receipt_text(&context.action_label, 200);
    if let Some(app) = &mut context.app {
        app.id = bounded_receipt_text(&app.id, 160);
        app.label = bounded_receipt_text(&app.label, 160);
    }
    if let Some(resource) = &mut context.resource {
        resource.id = bounded_receipt_text(&resource.id, 512);
        resource.kind = bounded_receipt_text(&resource.kind, 160);
        resource.label = bounded_receipt_text(&resource.label, 200);
    }
    context.purpose = context
        .purpose
        .as_deref()
        .map(|value| bounded_receipt_text(value, 512))
        .filter(|value| !value.is_empty());
    if let Some(group) = &mut context.group {
        group.id = bounded_receipt_text(&group.id, 160);
        group.label = bounded_receipt_text(&group.label, 200);
        group.kind = bounded_receipt_text(&group.kind, 160);
    }
    context.facets.truncate(32);
    for facet in &mut context.facets {
        facet.key = bounded_receipt_text(&facet.key, 96);
        facet.value = bounded_receipt_text(&facet.value, 160);
    }
}

#[cfg(test)]
mod tests {
    use collab_types::caps::{Scope, Subject, Verb};

    use super::*;
    use crate::ids::{RoomId, WorkspaceId};
    use crate::workspace::Workspace;

    fn tree() -> (DesktopTree, WorkspaceId) {
        let id = WorkspaceId(1);
        let mut workspace = Workspace::new(id, "Home");
        workspace.room = RoomId::new();
        workspace.set_owner("owner");
        (
            DesktopTree {
                workspaces: vec![workspace],
                ..DesktopTree::default()
            },
            id,
        )
    }

    fn app_grant() -> CapOverride {
        CapOverride::grant(
            Subject::app("so.tilo.caller"),
            Scope::Action {
                app: "so.tilo.target".into(),
                action: "open".into(),
            },
            Verb::Write,
        )
    }

    #[test]
    fn only_an_administrator_grants_and_the_stored_capability_names_them() {
        let (mut tree, id) = tree();
        assert!(
            tree.grant_in_workspace_at(id, &"stranger".to_string(), app_grant(), 10)
                .is_none()
        );
        let stored = tree
            .grant_in_workspace_at(id, &"owner".to_string(), app_grant(), 10)
            .unwrap();
        assert_eq!(
            stored.granted_by,
            Some(collab_types::caps::Principal::Person("owner".into()))
        );
        assert!(
            tree.workspaces[0]
                .capabilities
                .iter()
                .any(|capability| capability.id == stored.id)
        );
        assert!(
            tree.grant_in_workspace_at(WorkspaceId(2), &"owner".to_string(), app_grant(), 10)
                .is_none()
        );
    }

    #[test]
    fn a_receipt_is_recorded_once_and_bounded() {
        let (mut tree, id) = tree();
        let invocation = InvocationId::new();
        let context =
            app_action_grant_context("room", "Caller\u{7}", "Target", &"x".repeat(400), "once");
        let receipt = tree
            .append_workspace_effect_receipt_at(
                id,
                invocation,
                "owner",
                context,
                EffectReceiptStatus::Succeeded,
                42,
            )
            .unwrap();
        assert!(receipt.context.action_label.chars().count() <= 200);
        assert!(!receipt.context.action_label.contains('\u{7}'));
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
            tree.append_workspace_effect_receipt_at(
                id,
                invocation,
                "owner",
                ActionContext::default(),
                EffectReceiptStatus::Succeeded,
                43,
            )
            .is_none()
        );
        assert_eq!(tree.workspaces[0].effect_receipts.len(), 1);
    }
}
