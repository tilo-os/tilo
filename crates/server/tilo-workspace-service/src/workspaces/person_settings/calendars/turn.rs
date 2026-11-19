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

use calendar_model::spans::{FreeAsked, SpanAsked};
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
use crate::workspaces::hosted_use::{DelegatedTurn, KeepersTurn};

/// What a turn asks of its person's calendars: the reads, and nothing that
/// writes to a calendar server.
pub(in crate::workspaces) const READS: [&str; 3] = [LIST, ENTRIES, FREE];

impl Workspaces {
    /// Whose turn `turn` is, where that person's settings are kept, and the
    /// calendar accounts they named its agent (or its verified root assistant,
    /// when a sub-agent has no direct calendar binding) for in the turn's
    /// workspace.
    ///
    /// # Errors
    /// [`ActionError::UnknownAction`] for a turn that is not its keeper's
    /// own, and for an agent (and any verified root assistant) its person
    /// named for no calendar account in this workspace.
    async fn calendars_of_turn(
        &self,
        turn: DelegatedTurn<'_>,
    ) -> Result<(KeepersTurn, PathBuf, Vec<Account>), ActionError> {
        let using = self.keepers_turn(&turn.turn).await?;
        // The person's word is kept by the workspace's id, as their device
        // gave it from that workspace.
        let workspace = turn.turn.workspace.to_string();
        let root = self.person_settings_root(&using.person);
        let all = accounts_at(&root);
        let for_agent = |agent: &str| -> Vec<Account> {
            let named = accounts_of_assistant(
                &root,
                NamedAgent {
                    workspace: &workspace,
                    agent,
                },
            );
            all.iter()
                .filter(|account| named.contains(&account.id))
                .cloned()
                .collect()
        };
        let mut accounts = for_agent(&using.agent);
        if accounts.is_empty()
            && let Some(root_agent) = turn
                .root_agent
                .filter(|id| !id.is_empty() && *id != using.agent)
        {
            accounts = for_agent(root_agent);
        }
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
    pub async fn calendars_offered<'a>(
        &self,
        turn: impl Into<DelegatedTurn<'a>>,
    ) -> Vec<CatalogueRow> {
        let turn = turn.into();
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
                    turn.turn.may_take(&using.workspace, &target)
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
    pub async fn calendars_for_turn<'a>(
        &self,
        turn: impl Into<DelegatedTurn<'a>>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionError> {
        let turn = turn.into();
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
        if !turn.turn.may_take(&using.workspace, target) {
            return Err(ActionError::NotPermitted);
        }
        let reach = self.inner.models.mail_reach();
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        reason = "in unit tests panicking is the assertion"
    )]

    use serde_json::{Value, json};
    use tilo_action_policy::{ActionKind, Answer, KindSet, Rule, TreePath};
    use tilo_actions::remote::{
        ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
        person_accounts::{self, AssistantAccess},
    };
    use tilo_actions::{ActionAddress, InvocationId};
    use tilo_user_identity::DeviceCert;

    use super::{ENTRIES, FREE, LIST, RESOURCE};
    use crate::workspaces::hosted_use::HostedTurn;
    use crate::workspaces::person_settings::say_assistant;
    use crate::workspaces::tests::{governor, test_host as test_server};

    const ROOT_AGENT: &str = "scout";
    const SUB_AGENT: &str = "sub-calendar-reader";

    fn on_turn(person: &DeviceCert, workspace: &str, agent: &str) -> Caller {
        Caller::authenticated(person.user_public_key, person.device_public_key)
            .in_workspace(workspace)
            .delegated_to(agent)
            .named(agent)
            .in_agent_session("agent-session:test")
            .on_turn(
                tilo_action_policy::turn::Turn::new("direct", "cause"),
                Vec::new(),
            )
    }

    fn asking(action: &str, argument: Value) -> InvokeRequest {
        let Value::Object(argument) = argument else {
            panic!("arguments must be a JSON object");
        };
        InvokeRequest {
            invocation: InvocationId::new(),
            target: RemoteActionTarget {
                provider: person_accounts::PROVIDER.to_owned(),
                address: ActionAddress::new(RESOURCE, action),
            },
            argument: InvokeArgument::Json(argument),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        }
    }

    #[tokio::test]
    async fn sub_agent_inherits_root_calendar_accounts_respects_bounds_and_revocation() {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_server(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces();
        let created = workspaces.create("Team").await.unwrap();
        let workspace = created.workspace;
        let workspace_id = created.workspace_id;
        let ada = &owner;
        let workspace_key = workspace_id.to_string();

        // Name ONLY the root assistant for the calendar account.
        let account_id = workspaces
            .keep_calendar_as_pulled(
                &ada.user_public_key,
                Some((workspace_key.as_str(), ROOT_AGENT)),
                &[],
            )
            .unwrap();

        let sub_caller = on_turn(ada, &workspace, SUB_AGENT);
        let keeper = hex::encode(ada.user_public_key);
        let base_turn = HostedTurn {
            workspace: workspace_id,
            caller: &sub_caller,
            keeper: &keeper,
            bound: None,
        };

        // Without a verified root agent, the sub-agent reaches nothing.
        assert!(workspaces.calendars_offered(&base_turn).await.is_empty());
        assert_eq!(
            workspaces
                .calendars_for_turn(&base_turn, &asking(LIST, json!({})))
                .await,
            Err(ActionCallError::UnknownAction)
        );

        // With the verified root assistant, the sub-agent inherits calendar reads.
        let sub_turn = base_turn.under_root(Some(ROOT_AGENT));
        let offered: Vec<String> = workspaces
            .calendars_offered(&sub_turn)
            .await
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect();
        assert_eq!(offered, vec![LIST, ENTRIES, FREE]);

        let listed = workspaces
            .calendars_for_turn(&sub_turn, &asking(LIST, json!({})))
            .await
            .expect("sub-agent lists inherited calendar account");
        assert_eq!(listed["accounts"][0]["id"], account_id.as_str());

        // Explicit narrowing still restricts the sub-agent.
        let narrow_rules = vec![Rule::new(
            TreePath::workspace(&workspace)
                .app(person_accounts::PROVIDER)
                .kind(RESOURCE)
                .resource(RESOURCE)
                .action(LIST),
            KindSet::of(ActionKind::Outside),
            Answer::Allow,
        )];
        let narrowed_sub = HostedTurn {
            bound: Some(&narrow_rules),
            ..base_turn
        }
        .under_root(Some(ROOT_AGENT));
        let narrowed_offered: Vec<String> = workspaces
            .calendars_offered(&narrowed_sub)
            .await
            .into_iter()
            .filter_map(|row| row.action.map(|action| action.address.action))
            .collect();
        assert_eq!(narrowed_offered, vec![LIST]);
        assert_eq!(
            workspaces
                .calendars_for_turn(
                    &narrowed_sub,
                    &asking(
                        ENTRIES,
                        json!({"from": "2026-11-16", "to": "2026-11-23", "zone": "UTC"}),
                    ),
                )
                .await,
            Err(ActionCallError::NotPermitted)
        );

        // Revoking root immediately cuts off descendants.
        let root_dir = workspaces.person_settings_root(&ada.user_public_key);
        say_assistant(
            &root_dir,
            &workspace_key,
            &AssistantAccess {
                account: account_id,
                agent: ROOT_AGENT.to_owned(),
                may: false,
            },
        )
        .unwrap();
        assert!(workspaces.calendars_offered(&sub_turn).await.is_empty());
        assert_eq!(
            workspaces
                .calendars_for_turn(&sub_turn, &asking(LIST, json!({})))
                .await,
            Err(ActionCallError::UnknownAction)
        );

        server.shutdown().await;
    }
}
