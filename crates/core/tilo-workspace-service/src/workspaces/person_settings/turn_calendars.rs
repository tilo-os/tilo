//! A person's calendar accounts, as a turn this server carries out reaches them: beginning a
//! sign-in on the provider's own page, asking an account's calendar server which calendars it
//! holds, and saying which of them show.
//!
//! The turn names what it wants and nothing more. The sign-in is finished on the provider's page,
//! where its person types their password, or by the entry their vault already keeps; neither the
//! password nor any other secret is ever an argument of a call or an answer to one. An account
//! connected this way is one the calendar bridge signs in with from the workspace's vault: the
//! turn reads its calendars, and the entries arrive in the Calendar app's records, which the turn
//! reads as any app's.
//!
//! The checks are the same as the mail's ([`super::turn_mail`]): the turn is one this server
//! carries out, the workspace's policy lets that agent reach its calendars, and an agent anything
//! narrows holds a rule that names the very action. Unknown and forbidden are one answer.

use serde_json::Value;
use tilo_action_policy::ActionKind;
use tilo_actions::remote::{
    ActionCallError, CatalogueRow, InvokeRequest, RemoteActionTarget, person_accounts,
};

use super::kept_sign_ins::Signing;
use super::sign_in;
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::HostedTurn;
use crate::workspaces::workspace_stores::Store;

/// The calendar actions a turn is offered, and the ones it may call.
pub(in crate::workspaces) const TURN_ACTIONS: [&str; 3] = [
    person_accounts::BEGIN_SIGN_IN,
    person_accounts::DISCOVER_CALENDARS,
    person_accounts::SET_CALENDARS,
];

impl Workspaces {
    /// The calendar actions `turn` is offered, as a catalogue lists them: each one it would not be
    /// refused. None for a turn this server does not carry out, and none for a narrowed agent no
    /// rule of which names it.
    pub(crate) async fn calendars_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let Ok(using) = self.asked_turn(turn).await else {
            return Vec::new();
        };
        let mut offered = Vec::new();
        for row in crate::workspaces::actions::calendar_rows() {
            let Some(action) = &row.action else {
                continue;
            };
            let target = RemoteActionTarget {
                provider: row.provider.clone(),
                address: action.address.clone(),
            };
            if !TURN_ACTIONS.contains(&target.address.action.as_str()) {
                continue;
            }
            if !self
                .turn_reaches(
                    turn,
                    Store::CALENDARS,
                    &target.address.action,
                    ActionKind::Undoable,
                )
                .await
            {
                continue;
            }
            if turn.may_take(&using.workspace, &target) {
                offered.push(row);
            }
        }
        offered
    }

    /// Answer the calendar `request` of `turn`: the sign-in begun, the account's calendars found,
    /// or the ones chosen to show. What a sign-in asks is typed on its provider's page and kept in
    /// the workspace's vault; no answer carries it.
    ///
    /// # Errors
    /// The turn is none this server carries out or its person does not reach the workspace's
    /// calendars ([`ActionCallError::UnknownAction`]), its agent is narrowed and holds no rule for
    /// the action ([`ActionCallError::NotPermitted`]), or the arguments do not read.
    pub(crate) async fn calendars_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let target = &request.target;
        if target.provider != person_accounts::PROVIDER
            || target.address.resource != person_accounts::RESOURCE
            || !TURN_ACTIONS.contains(&target.address.action.as_str())
        {
            return Err(ActionCallError::UnknownAction);
        }
        let using = self.asked_turn(turn).await?;
        let action = target.address.action.as_str();
        if !self
            .turn_reaches(turn, Store::CALENDARS, action, ActionKind::Undoable)
            .await
            || !turn.may_take(&using.workspace, target)
        {
            return Err(ActionCallError::NotPermitted);
        }
        if action == person_accounts::BEGIN_SIGN_IN {
            // A turn connects an account for its calendars alone: the person's mail and files are
            // not reached with the calendars' grant.
            let signing_in: person_accounts::BeginSignIn =
                super::argument(request, "a provider to sign in with")?;
            let services = signing_in.services();
            if !services.calendar || services.mail || services.files {
                return Err(ActionCallError::UnknownAction);
            }
            return sign_in::accounts_sign_in(self, using.person, &using.workspace, request).await;
        }
        let root = self.workspace_root(&using.workspace);
        let asker = using.acting();
        let signing = Signing {
            vault: self.vault_for(&using.workspace, &asker).serving(request),
            root: &root,
            by: &using.agent,
        };
        // The sign-in is released for the account's server alone and written down as its use: the
        // same path Settings takes, with the arguments checked the same way.
        match self
            .person_mail_action(signing, &using.workspace, &asker, request)
            .await?
        {
            Some(answered) => Ok(answered),
            None => Err(ActionCallError::UnknownAction),
        }
    }
}
