//! A workspace's calendars, as a turn this server carries out reads them.
//!
//! An agent this server runs works with every device of whoever asked it off, so no transport
//! certified a device behind what it asks, and the check a device's request passes
//! ([`Workspaces::calendars_asked`]) has nothing to compare. This is the server's own entry for
//! such a turn, bounded as the workspace's mail is ([`Workspaces::mail_for_turn`]):
//! the turn is one this server carries out ([`Workspaces::asked_turn`]), and the workspace's policy
//! lets that agent read its calendars. An agent that anything narrows also holds a rule that names
//! the very action ([`HostedTurn::may_take`]). Unknown and forbidden are the same answer.
//!
//! Such a turn reads: which accounts and calendars there are, what they hold
//! in a span, and when their person is free ([`READS`]). Nothing that
//! writes to a calendar server is here. A turn files nothing and puts
//! nothing in: asking for either is asking for an action nobody knows.
//!
//! What an entry says was written by whoever made it, often somebody else.
//! Whoever hands an answer of [`ENTRIES`] to a model says so with it.

use std::path::PathBuf;

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
    person_accounts,
};
use tilolang_calendar::spans::{FreeAsked, SpanAsked};

use super::{
    Account, Asking, ENTRIES, FREE, FRESH_MS, LIST, Listed, RESOURCE, accounts_in, answer,
    argument, entries, free, off_thread, view,
};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::workspace_stores::Store;

/// What a turn asks of its person's calendars: the reads, and nothing that
/// writes to a calendar server.
pub(in crate::workspaces) const READS: [&str; 3] = [LIST, ENTRIES, FREE];

impl Workspaces {
    /// Whose turn `turn` is, where its workspace keeps its calendars, and
    /// the calendar accounts it keeps.
    ///
    /// # Errors
    /// [`ActionError::UnknownAction`] for a turn this server does not carry out, for an agent the
    /// workspace's policy does not let read its calendars, and for a workspace that keeps no
    /// calendar account.
    async fn calendars_of_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<(AskedTurn, PathBuf, Vec<Account>), ActionError> {
        let using = self.asked_turn(turn).await?;
        if !self
            .turn_reaches(
                turn,
                Store::CALENDARS,
                LIST,
                tilo_action_policy::ActionKind::Read,
            )
            .await
        {
            return Err(ActionError::UnknownAction);
        }
        let root = self.workspace_root(&using.workspace);
        let accounts = accounts_in(
            &self
                .workspace_accounts(&using.workspace)
                .await
                .map_err(|reason| ActionError::Refused { reason })?,
        );
        if accounts.is_empty() {
            return Err(ActionError::UnknownAction);
        }
        Ok((using, root, accounts))
    }

    /// The reads of its person's calendars `turn` is offered, as a catalogue
    /// lists them: each one it would not be refused. None for a turn this server does not carry out
    /// and for an agent that may not read the workspace's calendars; a narrowed agent is offered the
    /// ones a rule of its names.
    pub(crate) async fn calendars_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let Ok((using, ..)) = self.calendars_of_turn(turn).await else {
            return Vec::new();
        };
        Self::calendar_read_rows()
            .into_iter()
            .filter(|row| {
                row.action.as_ref().is_some_and(|action| {
                    let target = RemoteActionTarget {
                        provider: row.provider.clone(),
                        address: action.address.clone(),
                    };
                    turn.may_take(&using.workspace, &target)
                })
            })
            .collect()
    }

    /// Answer the read `request` asks of its person's calendars, for `turn`:
    /// as an agent is answered behind a device of its person, from the
    /// accounts the workspace keeps. A copy older than a minute is brought
    /// up to date first, which asks the calendar server to read and nothing
    /// else.
    ///
    /// # Errors
    /// The turn is none this server carries out, its person may not read
    /// the workspace's calendars, or the action is none
    /// of [`READS`] ([`ActionError::UnknownAction`]); its agent is narrowed
    /// and holds no rule for the action ([`ActionError::NotPermitted`]); or
    /// the arguments do not read.
    pub(crate) async fn calendars_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionError> {
        let target = &request.target;
        if target.provider != person_accounts::PROVIDER || target.address.resource != RESOURCE {
            return Err(ActionError::UnknownAction);
        }
        let (using, root, accounts) = self.calendars_of_turn(turn).await?;
        let action = target.address.action.as_str();
        // What writes to a calendar server is not here for a turn.
        if !READS.contains(&action) {
            return Err(ActionError::UnknownAction);
        }
        if !turn.may_take(&using.workspace, target) {
            return Err(ActionError::NotPermitted);
        }
        let asker = using.acting();
        let reach = Asking::of(
            self.vault_for(&using.workspace, &asker),
            &using.agent,
            self.inner.models.mail_reach(),
        );
        match action {
            LIST => answer(&Ok::<_, String>(Listed {
                accounts: accounts
                    .iter()
                    .map(|account| view(&root, account))
                    .collect(),
            })),
            ENTRIES => {
                let asked: SpanAsked = argument(request, "a span to read")?;
                answer(
                    &off_thread(move || entries(&root, &reach, &accounts, &asked, FRESH_MS)).await,
                )
            }
            FREE => {
                let asked: FreeAsked = argument(request, "a span to find free time in")?;
                answer(&off_thread(move || free(&root, &reach, &accounts, &asked)).await)
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}
