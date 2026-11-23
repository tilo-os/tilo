//! What this host says of the actions a window publishes, for the person at the window.
//!
//! The window decides no permission itself. Every action of the surface it publishes is decided
//! here with the actor's ordinary answer in the workspace holding the record (the window's, or
//! another: a person's profile record is kept in the person's workspace), and decided again when the
//! window publishes another surface or grants change. The window hears every verdict once and
//! then only those that changed; its controls show them, and a press runs as its verdict admits it.

use std::collections::HashMap;

use runtime_collab::shown_actions::{ActionDecision, ActionVerdict, ShownAction};
use tokio::sync::mpsc;

use super::wire::Feed;
use super::{AppBrokerState, BrokerActor};

pub(super) use tilo_broker_core::shown::NOT_HERE;
/// What a person reads when the holding workspace could not be read just now.
const UNREADABLE: &str = "This can't be checked right now. Try again.";

/// One window's verdicts, decided again when its surface or the grants change.
pub(super) struct ShownSubscription {
    actor: BrokerActor,
    /// The connection publishing the window's surface.
    peer: u64,
    /// The app the window is.
    app: String,
    /// The window's workspace, which holds every record not naming another.
    home: String,
    /// The verdicts last sent.
    last: HashMap<ShownAction, ActionVerdict>,
    tx: mpsc::UnboundedSender<Vec<ActionDecision>>,
}

impl AppBrokerState {
    /// The actions of the surface the connection `peer` publishes, as the app `app` publishes
    /// them: each at what it needs, and a field edit that is offered to whoever may react also at
    /// what any other edit of the record needs.
    fn published_actions(&self, peer: u64, app: &str) -> Vec<ShownAction> {
        let Ok(tables) = self.surface_tables.lock() else {
            return Vec::new();
        };
        let Some(table) = tables.get(&peer) else {
            return Vec::new();
        };
        let mut shown = Vec::new();
        for published in table.actions() {
            let workspace = table.workspace_of(&published.resource);
            let need = published.contract.need();
            let key = (
                published.resource_kind.as_str(),
                published.resource.as_str(),
                published.action.as_str(),
            );
            let field_edit = published
                .contract
                .tags
                .iter()
                .any(|tag| tag == tilo_action_policy::edits::FIELD_EDIT_TAG);
            if need.field.is_some() && field_edit {
                let edit = tilo_actions::ActionNeed {
                    field: None,
                    ..need.clone()
                };
                shown.push(ShownAction::new(workspace, app, key, &edit));
            }
            shown.push(ShownAction::new(workspace, app, key, &need));
        }
        // Whether the window may change what its app keeps at all, or read it.
        for kind in tilo_actions::ActionKind::ALL {
            shown.push(ShownAction::of_app(app, kind));
        }
        // What the workspace itself offers through any window: inviting someone, its settings.
        let governance = tilo_action_policy::roles::GOVERNANCE_KIND;
        for action in [
            tilo_action_policy::roles::governance::INVITE,
            tilo_action_policy::roles::governance::SETTINGS,
        ] {
            shown.push(ShownAction::of_workspace(action, governance));
        }
        shown
    }

    /// What this host says of `actor` running each of `actions`, shown in a window open in
    /// `home`, now.
    fn decide_shown_actions(
        &self,
        actor: &BrokerActor,
        home: &str,
        actions: Vec<ShownAction>,
    ) -> HashMap<ShownAction, ActionVerdict> {
        let now = self
            .policy_now_ns()
            .map(|ns| ns / 1_000_000_000)
            .inspect_err(|error| {
                tracing::warn!(%error, "shown actions cannot be decided without the policy clock");
            })
            .ok();
        actions
            .into_iter()
            .map(|action| {
                let verdict = now.map_or_else(
                    || ActionVerdict::Refused(UNREADABLE.into()),
                    |now| self.shown_verdict(actor, home, &action, now),
                );
                (action, verdict)
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

    /// The verdicts of every action the connection `peer` (a window of the app `app` open in
    /// `home`) publishes, for `actor`: all of them now, then those that change, for as long as
    /// the feed is kept.
    pub(super) fn subscribe_shown_actions(
        &self,
        actor: BrokerActor,
        (peer, app): (u64, String),
        home: String,
    ) -> Feed<Vec<ActionDecision>> {
        let (tx, rx) = mpsc::unbounded_channel();
        let last = self.decide_shown_actions(&actor, &home, self.published_actions(peer, &app));
        let _ = tx.send(decisions(&last));
        let subscription = ShownSubscription {
            actor,
            peer,
            app,
            home,
            last,
            tx,
        };
        match self.shown_actions.lock() {
            Ok(mut shown) => shown.push(subscription),
            Err(poisoned) => poisoned.into_inner().push(subscription),
        }
        Feed::new(rx)
    }

    /// Decide every window's actions again, as a policy change may have changed them.
    pub(super) fn refresh_shown_actions(&self) {
        self.refresh_shown_actions_where(|_| true);
    }

    /// Decide the actions of the window on the connection `peer` again: it published others.
    pub(super) fn refresh_shown_actions_of(&self, peer: u64) {
        self.refresh_shown_actions_where(|subscription| subscription.peer == peer);
    }

    /// Decide again the actions of the windows `which` picks, and send each the verdicts that
    /// changed. A window that let go of its feed is forgotten.
    fn refresh_shown_actions_where(&self, which: impl Fn(&ShownSubscription) -> bool) {
        let mut shown = match self.shown_actions.lock() {
            Ok(shown) => shown,
            Err(poisoned) => poisoned.into_inner(),
        };
        shown.retain(|subscription| !subscription.tx.is_closed());
        for subscription in shown.iter_mut().filter(|subscription| which(subscription)) {
            let now = self.decide_shown_actions(
                &subscription.actor,
                &subscription.home,
                self.published_actions(subscription.peer, &subscription.app),
            );
            let changed: Vec<ActionDecision> = now
                .iter()
                .filter(|(action, verdict)| subscription.last.get(*action) != Some(*verdict))
                .map(|(action, verdict)| ActionDecision {
                    action: action.clone(),
                    verdict: verdict.clone(),
                })
                .collect();
            subscription.last = now;
            if !changed.is_empty() {
                let _ = subscription.tx.send(changed);
            }
        }
    }
}

/// `verdicts` as the decisions a window reads.
fn decisions(verdicts: &HashMap<ShownAction, ActionVerdict>) -> Vec<ActionDecision> {
    verdicts
        .iter()
        .map(|(action, verdict)| ActionDecision {
            action: action.clone(),
            verdict: verdict.clone(),
        })
        .collect()
}
