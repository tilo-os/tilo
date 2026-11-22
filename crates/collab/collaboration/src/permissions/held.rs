//! What the workspaces holding records a window shows, other than the window's own, say of
//! those records' actions: a person's profile record is held in their own workspace, which
//! decides its changes wherever it is shown.
//!
//! The window holds no other workspace's policy. Its host decides each such action and pushes
//! the verdicts here ([`runtime_collab::held_elsewhere`]); they only say how the window shows its
//! controls. Running one is admitted by the host against the holding workspace as it is then.

use std::collections::BTreeMap;

use runtime_collab::held_elsewhere::{HeldDecision, HeldRecord, HeldVerdict};
use tilo_action_policy::Answer;

/// What a person reads at a control of such a record while its verdict is on its way.
pub const CHECKING: &str = "Checking with the workspace that keeps this.";

/// The actions of the records held elsewhere that a window asked its host about, each with the
/// verdict the host last sent, or none yet.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HeldVerdicts {
    /// By resource and action.
    verdicts: BTreeMap<(String, String), (HeldRecord, Option<HeldVerdict>)>,
}

impl HeldVerdicts {
    /// The actions `records` names, each keeping the verdict it had when it is asked about as
    /// before, and none yet otherwise.
    #[must_use]
    pub fn asking(&self, records: &[HeldRecord]) -> Self {
        let verdicts = records
            .iter()
            .map(|record| {
                let key = (record.resource.clone(), record.action.clone());
                let verdict = self
                    .verdicts
                    .get(&key)
                    .filter(|(asked, _)| asked == record)
                    .and_then(|(_, verdict)| verdict.clone());
                (key, (record.clone(), verdict))
            })
            .collect();
        Self { verdicts }
    }

    /// Take the host's `decisions`: those of actions asked about as they are now.
    pub fn decide(&mut self, decisions: &[HeldDecision]) {
        for decision in decisions {
            let key = (
                decision.record.resource.clone(),
                decision.record.action.clone(),
            );
            if let Some((asked, verdict)) = self.verdicts.get_mut(&key)
                && *asked == decision.record
            {
                *verdict = Some(decision.verdict.clone());
            }
        }
    }

    /// The records asked about.
    pub fn records(&self) -> impl Iterator<Item = &HeldRecord> {
        self.verdicts.values().map(|(record, _)| record)
    }

    /// What the window shows of `action` on `resource`, when the record is held elsewhere and it
    /// was asked about: refused until its verdict arrives.
    #[must_use]
    pub fn answer(&self, resource: &str, action: &str) -> Option<Answer> {
        let (_, verdict) = self
            .verdicts
            .get(&(resource.to_owned(), action.to_owned()))?;
        Some(match verdict {
            Some(HeldVerdict::Allowed) => Answer::Allow,
            Some(HeldVerdict::AsksFirst) => Answer::Ask,
            Some(HeldVerdict::Refused(_)) | None => Answer::Deny,
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
            (_, Some(HeldVerdict::Refused(why))) => Some(why),
            (_, Some(_)) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::permissions::{MemberId, PermissionContext, Role, SharedPermissions};
    use tilo_actions::{ActionKind, ActionSpec, OperationKind, ResourceSpec};

    /// A board's viewer sees their own record's change refused until their own workspace's
    /// verdict arrives, then as it says, and keeps it when their role on the board changes.
    #[test]
    fn a_record_held_elsewhere_is_shown_as_its_workspace_says() {
        let person = MemberId::person("ad".repeat(32));
        let mut mute =
            ActionSpec::operation("mute", "Mute", OperationKind::Effect, "t.mute", &()).unwrap();
        mute.type_mut().contract.kind = ActionKind::Undoable;
        let mut playback = ResourceSpec::new("person:test.playback", "test.playback", "Playback")
            .with_action(mute.clone());
        playback.workspace = Some("own".into());
        let record = HeldRecord::new(
            "own",
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
        // Not asked yet, and asked but not answered: refused, saying it is being checked.
        assert_eq!(answer(&shared), Answer::Deny);
        shared.update(|context| context.ask_held(std::slice::from_ref(&record)));
        assert_eq!(answer(&shared), Answer::Deny);
        assert!(
            shared
                .borrow()
                .permitted_resource("board", playback.clone())
                .is_none()
        );
        let refused = shared.borrow().held_refusal(&playback, &mute);
        assert!(
            matches!(&refused, Some(tilo_actions::ApiError::Unavailable { reason, .. }) if reason == CHECKING),
            "{refused:?}"
        );
        shared.update(|context| {
            context.decide_held(&[HeldDecision {
                record: record.clone(),
                verdict: HeldVerdict::Allowed,
            }]);
        });
        assert_eq!(answer(&shared), Answer::Allow);
        assert!(shared.borrow().action_decider("board").permits(
            "test.playback",
            "person:test.playback",
            &mute
        ));
        // The board's roles change; what the person's own workspace said stays.
        shared.set(PermissionContext::member(&person, Role::Commenter));
        assert_eq!(answer(&shared), Answer::Allow);
        shared.update(|context| {
            context.decide_held(&[HeldDecision {
                record,
                verdict: HeldVerdict::Refused("no".into()),
            }]);
        });
        assert_eq!(answer(&shared), Answer::Deny);
    }
}
