//! A person's calendars, as a turn this server carries out for them reads
//! them.
//!
//! An agent a person keeps on this server works with every device of theirs
//! off, so no transport certified a device behind what it asks, and the
//! check a device's request passes ([`Workspaces::calendars_asked`]) has
//! nothing to compare. This is the server's own entry for such a turn,
//! bounded as their mail's is
//! ([`crate::workspaces::hosted_use::TurnMail`]): the turn is its keeper's
//! own ([`Workspaces::keepers_turn`]), and that person named this agent for
//! the calendar account, in this workspace. Nothing is granted by default,
//! and the same id in a workspace somebody else keeps is nobody. An agent
//! that anything narrows also holds a rule that names the very action
//! ([`HostedTurn::may_take`]). Unknown and forbidden are the same answer.
//!
//! Such a turn reads: which accounts and calendars there are, what they hold
//! in a span, and when their person is free ([`READS`]). Nothing that
//! writes to a calendar server is here. A turn files nothing and puts
//! nothing in: asking for either is asking for an action nobody knows.
//!
//! What an entry says was written by whoever made it, often somebody else.
//! Whoever hands an answer of [`ENTRIES`] to a model says so with it.

use std::path::PathBuf;

use calendar::spans::{FreeAsked, SpanAsked};
use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
    person_accounts,
};

use super::{
    Account, ENTRIES, FREE, FRESH_MS, LIST, Listed, NamedAgent, RESOURCE, accounts_at,
    accounts_of_assistant, answer, argument, entries, free, off_thread, view,
};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{HostedTurn, KeepersTurn};

/// What a turn asks of its person's calendars: the reads, and nothing that
/// writes to a calendar server.
pub(in crate::workspaces) const READS: [&str; 3] = [LIST, ENTRIES, FREE];

impl Workspaces {
    /// Whose turn `turn` is, where that person's settings are kept, and the
    /// calendar accounts they named its agent for in the turn's workspace.
    ///
    /// # Errors
    /// [`ActionError::UnknownAction`] for a turn that is not its keeper's
    /// own, and for an agent its person named for no calendar account in
    /// this workspace.
    async fn calendars_of_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<(KeepersTurn, PathBuf, Vec<Account>), ActionError> {
        let using = self.keepers_turn(turn).await?;
        // The person's word is kept by the workspace's id, as their device
        // gave it from that workspace.
        let workspace = turn.workspace.to_string();
        let root = self.person_settings_root(&using.person);
        let named = accounts_of_assistant(
            &root,
            NamedAgent {
                workspace: &workspace,
                agent: &using.agent,
            },
        );
        let mut accounts = accounts_at(&root);
        accounts.retain(|account| named.contains(&account.id));
        if accounts.is_empty() {
            return Err(ActionError::UnknownAction);
        }
        Ok((using, root, accounts))
    }

    /// The reads of its person's calendars `turn` is offered, as a catalogue
    /// lists them: each one it would not be refused. None for a turn that is
    /// not its keeper's own and for an agent its person named for no
    /// calendar account in this workspace; a narrowed agent is offered the
    /// ones a rule of its names.
    pub async fn calendars_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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
    /// as the named agent is answered behind a device of its person, from
    /// the accounts it was named for. A copy older than a minute is brought
    /// up to date first, which asks the calendar server to read and nothing
    /// else.
    ///
    /// # Errors
    /// The turn is none this server carries out for the calendars' person,
    /// its agent is not named for a calendar account, or the action is none
    /// of [`READS`] ([`ActionError::UnknownAction`]); its agent is narrowed
    /// and holds no rule for the action ([`ActionError::NotPermitted`]); or
    /// the arguments do not read.
    pub async fn calendars_for_turn(
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
        match action {
            LIST => answer(&Ok::<_, String>(Listed {
                accounts: accounts
                    .iter()
                    .map(|account| view(&root, account))
                    .collect(),
            })),
            ENTRIES => {
                let asked: SpanAsked = argument(request, "a span to read")?;
                answer(&off_thread(move || entries(&root, &accounts, &asked, FRESH_MS)).await)
            }
            FREE => {
                let asked: FreeAsked = argument(request, "a span to find free time in")?;
                answer(&off_thread(move || free(&root, &accounts, &asked)).await)
            }
            _ => Err(ActionError::UnknownAction),
        }
    }
}
