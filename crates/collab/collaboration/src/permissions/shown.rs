//! What the host says of the actions a window shows.
//!
//! The window decides none of them. The device's daemon decides each action the window publishes
//! with the ordinary check in the workspace holding the record and sends the verdicts here as they
//! change ([`runtime_collab::shown_actions`]): every one first, then each that changed.

use std::collections::HashMap;

use runtime_collab::shown_actions::{ActionDecision, ActionVerdict, ShownAction};

/// What a person reads at a control while its verdict is on its way.
pub const CHECKING: &str = "Checking whether you can do this.";

/// The verdict the host last sent of each action, by resource, then action and the field it
/// was decided at.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownVerdicts {
    verdicts: HashMap<String, Vec<(String, Option<String>, ActionVerdict)>>,
}

impl ShownVerdicts {
    /// Take the host's `decisions`, each replacing what it said of the same action before.
    pub fn decide(&mut self, decisions: &[ActionDecision]) {
        for decision in decisions {
            let action = &decision.action;
            let of = self.verdicts.entry(action.resource.clone()).or_default();
            match of
                .iter_mut()
                .find(|(id, field, _)| *id == action.action && *field == action.field)
            {
                Some((_, _, verdict)) => verdict.clone_from(&decision.verdict),
                None => of.push((
                    action.action.clone(),
                    action.field.clone(),
                    decision.verdict.clone(),
                )),
            }
        }
    }

    /// Whether the host said nothing yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }

    /// What the host last said of `action`.
    #[must_use]
    pub fn verdict(&self, action: &ShownAction) -> Option<&ActionVerdict> {
        self.verdict_at(&action.resource, &action.action, action.field.as_deref())
    }

    /// What the host last said of the action `action` decided at `field`, on any resource.
    pub fn verdicts_of<'a>(
        &'a self,
        action: &'a str,
        field: Option<&'a str>,
    ) -> impl Iterator<Item = &'a ActionVerdict> + 'a {
        self.verdicts
            .values()
            .flatten()
            .filter_map(move |(id, at, verdict)| {
                (id == action && at.as_deref() == field).then_some(verdict)
            })
    }

    /// What the host last said of the action `action` on `resource`, decided at `field`.
    #[must_use]
    pub fn verdict_at(
        &self,
        resource: &str,
        action: &str,
        field: Option<&str>,
    ) -> Option<&ActionVerdict> {
        self.verdicts
            .get(resource)?
            .iter()
            .find(|(id, at, _)| id == action && at.as_deref() == field)
            .map(|(_, _, verdict)| verdict)
    }
}

/// Why a control whose action has `verdict` cannot be used now, in words a person reads: the
/// verdict's, or that it is still being checked. `None` when it can.
#[must_use]
pub fn refusal(verdict: Option<&ActionVerdict>) -> Option<&str> {
    match verdict {
        None => Some(CHECKING),
        Some(ActionVerdict::Refused(why)) => Some(why),
        Some(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{MemberId, PermissionContext, Role, SharedPermissions, shown_action};
    use tilo_actions::{ActionKind, ActionSpec, OperationKind, ResourceSpec};

    /// A window shows an action as its host says once the verdict came, waits for it until then,
    /// and keeps the host's verdict when its own context changes: only the host changes it.
    #[test]
    fn a_shown_action_is_shown_as_the_host_says() {
        let person = MemberId::person("ad".repeat(32));
        let mut mute =
            ActionSpec::operation("mute", "Mute", OperationKind::Effect, "t.mute", &()).unwrap();
        mute.type_mut().contract.kind = ActionKind::Undoable;
        let playback = ResourceSpec::new("person:test.playback", "test.playback", "Playback")
            .with_action(mute.clone());
        let shared = SharedPermissions::new(PermissionContext::new(person.clone()));
        let admit = |shared: &SharedPermissions| {
            shared
                .borrow()
                .admit("board", &playback, &mute, (None, None), false)
        };
        assert!(matches!(
            admit(&shared),
            Err(tilo_actions::ApiError::Unavailable { code, .. }) if code == tilo_actions::api::NOT_READY
        ));
        assert!(
            shared
                .borrow()
                .shown_resource("board", playback.clone())
                .is_some(),
            "an action still being decided stays, and its press waits"
        );
        let action = shown_action("board", &playback, &mute);
        shared.decide_shown(&[ActionDecision {
            action: action.clone(),
            verdict: ActionVerdict::Allowed,
        }]);
        assert_eq!(admit(&shared), Ok(()));
        shared.set(PermissionContext::member(&person, Role::Viewer));
        assert_eq!(admit(&shared), Ok(()), "the host's verdict stays");
        shared.decide_shown(&[ActionDecision {
            action,
            verdict: ActionVerdict::Refused("no".into()),
        }]);
        assert!(matches!(
            admit(&shared),
            Err(tilo_actions::ApiError::Unavailable { reason, .. }) if reason == "no"
        ));
        assert!(
            shared
                .borrow()
                .shown_resource("board", playback.clone())
                .is_none()
        );
    }

    /// A harness host decides as the workspace's grants say: a viewer may read, not change, and
    /// an editor reaching outside is asked first.
    #[test]
    fn a_harness_host_decides_by_the_workspace_policy() {
        let person = MemberId::person("ad".repeat(32));
        let action = |kind| {
            let mut spec =
                ActionSpec::operation("do", "Do", OperationKind::Effect, "t.do", &()).unwrap();
            spec.type_mut().contract.kind = kind;
            let resource =
                ResourceSpec::new("notes.page", "page:1", "Plan").with_action(spec.clone());
            (resource, spec)
        };
        let verdict = |role, kind| {
            let (resource, spec) = action(kind);
            PermissionContext::member(&person, role).verdict_of("so.tilo.notes", &resource, &spec)
        };
        assert_eq!(
            verdict(Role::Viewer, ActionKind::Read),
            Some(ActionVerdict::Allowed)
        );
        assert!(matches!(
            verdict(Role::Viewer, ActionKind::Undoable),
            Some(ActionVerdict::Refused(_))
        ));
        assert_eq!(
            verdict(Role::Editor, ActionKind::Undoable),
            Some(ActionVerdict::Allowed)
        );
        assert_eq!(
            verdict(Role::Owner, ActionKind::Outside),
            Some(ActionVerdict::Allowed)
        );
    }

    /// An agent granted one field changes that field and nothing else of the app's records.
    #[test]
    fn an_agent_granted_one_field_changes_only_that_field() {
        use crate::permissions::{KindSet, TreePath};
        use tilo_actions::ActionNeed;
        const APP: &str = "so.tilo.test-memo";
        let agent = PermissionContext::agent_granted(
            "scribe",
            &TreePath::root().app(APP).rest().field("title"),
            KindSet::READ_UNDOABLE,
        );
        let edit = |field: Option<&str>, action: &str| {
            let need = ActionNeed {
                field: field.map(str::to_owned),
                ..ActionNeed::of(ActionKind::Undoable)
            };
            agent.verdict(&ShownAction::new(
                None,
                APP,
                ("test.memo", "test.memo", action),
                &need,
            ))
        };
        assert_eq!(edit(Some("title"), "retitle"), Some(ActionVerdict::Allowed));
        assert!(
            !matches!(edit(None, "add-line"), Some(ActionVerdict::Allowed)),
            "{:?}",
            edit(None, "add-line")
        );
        assert!(
            !agent.allows(&ShownAction::of_app(APP, ActionKind::Undoable)),
            "{:?}",
            agent.verdict(&ShownAction::of_app(APP, ActionKind::Undoable))
        );
    }
}
