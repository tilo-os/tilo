//! What this host says of the actions a window shows, for the person at the window.
//!
//! The window decides no permission itself. It names the actions of the records it shows
//! ([`tilo_permissions::shown_actions::ShownAction`]); the broker decides each with the actor's
//! ordinary answer in the workspace holding the record (the window's, or another: a person's
//! profile record is held in their own workspace), and decides again on every policy change,
//! sending the decisions when they differ. They say how the window shows its controls; an
//! invocation is admitted on its own when it is called.

use tilo_permissions::shown_actions::ActionDecision;
use tilo_permissions::shown_actions::ActionVerdict;
use tilo_permissions::shown_actions::ShownAction;
use tokio::sync::mpsc;

use super::wire::Feed;
use super::{AppBrokerState, BrokerActor};

pub(super) use tilo_broker_core::shown::NOT_HERE;
/// What a person reads when the holding workspace could not be read just now.
const UNREADABLE: &str = "This can't be checked right now. Try again.";

/// One window's shown actions, decided again on each policy change.
pub(super) struct ShownSubscription {
    actor: BrokerActor,
    /// The window's workspace, which holds every record not naming another.
    home: String,
    actions: Vec<ShownAction>,
    last: Vec<ActionDecision>,
    tx: mpsc::UnboundedSender<Vec<ActionDecision>>,
}

impl AppBrokerState {
    /// What this host says of `actor` running each of `actions`, shown in a window open in
    /// `home`, now.
    pub(super) fn decide_shown_actions(
        &self,
        actor: &BrokerActor,
        home: &str,
        actions: &[ShownAction],
    ) -> Vec<ActionDecision> {
        let now = self
            .policy_now_ns()
            .map(|ns| ns / 1_000_000_000)
            .inspect_err(|error| {
                tracing::warn!(%error, "shown actions cannot be decided without the policy clock");
            })
            .ok();
        actions
            .iter()
            .map(|action| ActionDecision {
                action: action.clone(),
                verdict: now.map_or_else(
                    || ActionVerdict::Refused(UNREADABLE.into()),
                    |now| self.shown_verdict(actor, home, action, now),
                ),
            })
            .collect()
    }

    fn shown_verdict(
        &self,
        actor: &BrokerActor,
        home: &str,
        action: &ShownAction,
        now: u64,
    ) -> ActionVerdict {
        let Some(membership) = self.membership.as_ref() else {
            return ActionVerdict::Refused(NOT_HERE.into());
        };
        let workspace = action.workspace.as_deref().unwrap_or(home);
        membership
            .decide_in_workspace(workspace, |held| actor.shown_verdict(held, action, now))
            .unwrap_or_else(|error| {
                tracing::warn!(%error, %workspace, "a shown action's workspace could not be read");
                ActionVerdict::Refused(UNREADABLE.into())
            })
    }

    /// The workspace holding the resource `resource` the connection `peer` publishes, when its
    /// surface names one other than the window's.
    pub(super) fn keeping_workspace(&self, peer: u64, resource: &str) -> Option<String> {
        self.surface_tables
            .lock()
            .ok()?
            .get(&peer)?
            .workspace_of(resource)
            .map(str::to_owned)
    }

    /// The decisions of `actions`, shown in a window open in `home`, for `actor` now, then each
    /// time a policy change makes them differ, for as long as the feed is held.
    pub(super) fn subscribe_shown_actions(
        &self,
        actor: BrokerActor,
        home: String,
        actions: Vec<ShownAction>,
    ) -> Feed<Vec<ActionDecision>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let last = self.decide_shown_actions(&actor, &home, &actions);
        let _ = tx.send(last.clone());
        let subscription = ShownSubscription {
            actor,
            home,
            actions,
            last,
            tx,
        };
        match self.shown_actions.lock() {
            Ok(mut shown) => shown.push(subscription),
            Err(poisoned) => poisoned.into_inner().push(subscription),
        }
        Feed::new(rx)
    }

    /// Decide every window's shown actions again, as a policy change may have changed them, and
    /// send those that differ. A window that let go of its feed is forgotten.
    pub(super) fn refresh_shown_actions(&self) {
        let mut shown = match self.shown_actions.lock() {
            Ok(shown) => shown,
            Err(poisoned) => poisoned.into_inner(),
        };
        shown.retain(|subscription| !subscription.tx.is_closed());
        for subscription in shown.iter_mut() {
            let now = self.decide_shown_actions(
                &subscription.actor,
                &subscription.home,
                &subscription.actions,
            );
            if now != subscription.last {
                subscription.last.clone_from(&now);
                let _ = subscription.tx.send(now);
            }
        }
    }
}
