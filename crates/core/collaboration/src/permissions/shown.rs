//! What the host says of the actions a window shows.
//!
//! The window decides none of them. Its host decides each with the ordinary check in the
//! workspace holding the record and pushes the verdicts here ([`runtime_collab::shown_actions`]);
//! they only say how the window shows its controls. Running one is admitted by the host when it
//! is called.

use std::collections::BTreeMap;

use runtime_collab::shown_actions::{ActionDecision, ActionVerdict, ShownAction};
use tilo_action_policy::Answer;

/// What a person reads at a control while its verdict is on its way.
pub const CHECKING: &str = "Checking whether you can do this.";

/// The actions a window asked its host about, each with the verdict the host last sent, or none
/// yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShownVerdicts {
    /// By resource and action.
    verdicts: BTreeMap<(String, String), (ShownAction, Option<ActionVerdict>)>,
}

impl ShownVerdicts {
    /// The `actions` asked about, each keeping the verdict it had when it is asked about as
    /// before, and none yet otherwise.
    #[must_use]
    pub fn asking(&self, actions: &[ShownAction]) -> Self {
        let verdicts = actions
            .iter()
            .map(|action| {
                let key = (action.resource.clone(), action.action.clone());
                let verdict = self
                    .verdicts
                    .get(&key)
                    .filter(|(asked, _)| asked == action)
                    .and_then(|(_, verdict)| verdict.clone());
                (key, (action.clone(), verdict))
            })
            .collect();
        Self { verdicts }
    }

    /// Take the host's `decisions`: those of actions asked about as they are now.
    pub fn decide(&mut self, decisions: &[ActionDecision]) {
        for decision in decisions {
            let key = (
                decision.action.resource.clone(),
                decision.action.action.clone(),
            );
            if let Some((asked, verdict)) = self.verdicts.get_mut(&key)
                && *asked == decision.action
            {
                *verdict = Some(decision.verdict.clone());
            }
        }
    }

    /// Whether nothing was asked about.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.verdicts.is_empty()
    }

    /// The actions asked about.
    pub fn actions(&self) -> impl Iterator<Item = &ShownAction> {
        self.verdicts.values().map(|(action, _)| action)
    }

    /// What the window shows of `action` on `resource`, when it was asked about: refused until
    /// its verdict arrives.
    #[must_use]
    pub fn answer(&self, resource: &str, action: &str) -> Option<Answer> {
        let (_, verdict) = self
            .verdicts
            .get(&(resource.to_owned(), action.to_owned()))?;
        Some(match verdict {
            Some(ActionVerdict::Allowed) => Answer::Allow,
            Some(ActionVerdict::AsksFirst) => Answer::Ask,
            Some(ActionVerdict::Refused(_)) | None => Answer::Deny,
        })
    }

    /// Why `action` on `resource` is refused here, in words a person reads: its verdict's, or
    /// that it is still being checked.
    #[must_use]
    pub fn refusal(&self, resource: &str, action: &str) -> Option<&str> {
        match self
            .verdicts
            .get(&(resource.to_owned(), action.to_owned()))?
        {
            (_, None) => Some(CHECKING),
            (_, Some(ActionVerdict::Refused(why))) => Some(why),
            (_, Some(_)) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{MemberId, PermissionContext, Role, SharedPermissions};
    use tilo_actions::{ActionKind, ActionSpec, OperationKind, ResourceSpec};

    /// A viewer's window shows an action as its host says once it asked about it, refused while
    /// the verdict is on its way, and keeps the host's verdict when the window's own access
    /// changes: only the host changes it.
    #[test]
    fn a_shown_action_is_shown_as_the_host_says() {
        let person = MemberId::person("ad".repeat(32));
        let mut mute =
            ActionSpec::operation("mute", "Mute", OperationKind::Effect, "t.mute", &()).unwrap();
        mute.type_mut().contract.kind = ActionKind::Undoable;
        let playback = ResourceSpec::new("person:test.playback", "test.playback", "Playback")
            .with_action(mute.clone());
        let action = ShownAction::new(
            None,
            "board",
            ("test.playback", "person:test.playback", "mute"),
            &mute.contract.need(),
        );
        let shared = SharedPermissions::new(PermissionContext::member(&person, Role::Viewer));
        let answer = |shared: &SharedPermissions| {
            shared
                .borrow()
                .invocation_answer("board", &playback, &mute, None, None)
        };
        shared.update(|context| context.ask_shown(std::slice::from_ref(&action)));
        assert_eq!(answer(&shared), Answer::Deny);
        assert!(
            shared
                .borrow()
                .permitted_resource("board", playback.clone())
                .is_none()
        );
        let refused = shared.borrow().shown_refusal(&playback, &mute);
        assert!(
            matches!(&refused, Some(tilo_actions::ApiError::Unavailable { reason, .. }) if reason == CHECKING),
            "{refused:?}"
        );
        shared.update(|context| {
            context.decide_shown(&[ActionDecision {
                action: action.clone(),
                verdict: ActionVerdict::Allowed,
            }]);
        });
        assert_eq!(answer(&shared), Answer::Allow);
        assert!(shared.borrow().action_decider("board").permits(
            "test.playback",
            "person:test.playback",
            &mute
        ));
        shared.set(PermissionContext::member(&person, Role::Commenter));
        assert_eq!(answer(&shared), Answer::Allow);
        shared.update(|context| {
            context.decide_shown(&[ActionDecision {
                action,
                verdict: ActionVerdict::Refused("no".into()),
            }]);
        });
        assert_eq!(answer(&shared), Answer::Deny);
        let refused = shared.borrow().shown_refusal(&playback, &mute);
        assert!(
            matches!(&refused, Some(tilo_actions::ApiError::Unavailable { reason, .. }) if reason == "no"),
            "{refused:?}"
        );
    }
}
