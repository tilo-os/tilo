//! A mail account its person tells their assistant of by its address, signed in on this server.
//!
//! The same asking a calendar is connected by ([`super::calendar_connect`]), for mail. A turn
//! hands over an address and nothing that is a secret; the server's entry
//! ([`crate::workspaces::Workspaces::connect_mail_for_turn`]) works out the account's two
//! servers and keeps what was asked. [`TurnMailConnects`] is that entry as one turn's tools
//! reach it, a provider of the turn's own ([`TurnProvider`]): it lists the one action beside the
//! workspace's resources, and takes it. The tool call ends at once, told that its person is
//! asked and which servers were found, and this puts the question to them: Chat's own, of the
//! kind that shows a protected field, in the words the entry says, which name both servers.
//!
//! The server reads their answer from the workspace's document at its next look
//! (`settle_mail_connects`), before the look reads which answers wake their askers: it claims
//! the ask, says on the question that it started and signs in on a task of its own. A password
//! a mail server does not take is asked for again by the server itself: the first question is
//! told which server refused it, a new question follows it, and no model is asked anything for
//! that. Once the account is signed in, or where signing in failed for another reason, the
//! agent that asked is woken once with what came of it. Neither the password nor the item's
//! reference passes a model on this way, nothing counts a person's tries, and no question ends
//! because time passed.
//!
//! A question that no longer says what the server asked (an agent may edit a message it wrote,
//! and may ask questions of its own) signs nothing in, and what was typed under it is not kept.

use std::sync::Arc;

use chat::question::QuestionState::{Answered, Settled, Waiting, Withdrawn};
use serde_json::Value;
use tilo_actions::policy::MemberId;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::calendar_connect::{
    ASKED_FIRST, Carrier, DECLINED, Found, PutAsk, Stands, TYPED, asked_in, put, question_about,
};
use super::mail::narrowing;
use super::turn_provider::{Ending, Rows};
use super::vault_uses::settle;
use super::{Reply, Turn, TurnProvider, WorkspaceActionProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{
    HostedTurn, MAIL_CONNECT, MailAsk, MailCarried, TurnMailConnect,
};

/// What an agent is told when its person is asked for the password: nothing is signed in yet,
/// and it neither waits nor asks again.
const ASKED: &str = "Your person is asked in your conversation with them for the password of \
     this mail account, in a protected field you cannot read. Nothing is signed in yet. Tell \
     them in one line to type it there. Once they have, this server signs the account in and \
     you are told; a password that is not accepted is asked for again by this server itself. \
     Do not make this call again.";

/// What an agent is told when its person was asked for this account's password before and has
/// not answered.
const WAITS: &str = "Your person was asked for the password of this mail account before, in a \
     protected field of your conversation with them, and has not typed it yet. Nothing is \
     signed in yet. Tell them it waits for them there, and do not make this call again.";

/// What a question is told when this server stopped while it signed the account in.
const INTERRUPTED: &str = "The server restarted while this mail account was being signed in. \
     Tell your assistant again if it is still wanted.";

impl PutAsk for MailAsk {
    fn by_agent(&self) -> &str {
        &self.agent
    }

    fn of_person(&self) -> &str {
        &self.person
    }

    fn shown(&self) -> Value {
        self.detail()
    }

    fn said(&self) -> String {
        self.words()
    }

    fn put_in(&self) -> &Value {
        &self.asked_in
    }
}

/// What one turn signs in of its person's mail. A turn's caller changes when its person writes
/// again while it works, so each telling is taken as the caller of the tool call that made it.
pub(super) struct TurnMailConnects {
    workspaces: Workspaces,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The signing in this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
    /// The workspace's own actions, through which the question is put.
    service: WorkspaceActionProvider,
}

impl TurnMailConnects {
    /// What `turn` signs in of its person's mail, where `reply` holds what its tools begin as
    /// and act through.
    pub(super) async fn for_turn(workspaces: &Workspaces, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = narrowing(&turn.agent, &reply.workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            keeper: &turn.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let offered = workspaces.mail_connect_offered(&hosted).await;
        Arc::new(Self {
            workspaces: workspaces.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
            service: reply.provider.clone(),
        })
    }

    /// Take the telling `request` makes as `caller`: its person is asked, or the server's
    /// refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = narrowing(&self.agent, &self.workspace);
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            keeper: &self.agent.keeper,
            bound: narrowed.as_deref(),
        };
        let workspaces = &self.workspaces;
        let taken = workspaces
            .connect_mail_for_turn(&hosted, request, asked_in(caller))
            .await?;
        let words = match taken {
            TurnMailConnect::Waits => WAITS.to_owned(),
            TurnMailConnect::Asks(open) => {
                match put(&self.service, caller, &self.workspace, open.as_ref()).await {
                    Ok(()) => {
                        workspaces.mail_connect_put(&open);
                        format!(
                            "{ASKED} What this server worked out from the address: {}.",
                            open.servers()
                        )
                    }
                    Err(why) => {
                        workspaces.mail_connect_not_asked(&open);
                        format!(
                            "Your person could not be asked for the password ({why}), so nothing \
                             was signed in."
                        )
                    }
                }
            }
        };
        Ok(InvocationUpdate::Failed {
            invocation: request.invocation,
            error: ActionFailure {
                code: ASKED_FIRST.to_owned(),
                message: words,
                details: None,
                retryable: false,
            },
        })
    }
}

impl TurnProvider for TurnMailConnects {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered the signing in.
    fn providers(&self, _caller: &Caller) -> Vec<CatalogueProvider> {
        if self.offered.is_empty() {
            return Vec::new();
        }
        vec![CatalogueProvider {
            provider: person_accounts::PROVIDER.to_owned(),
            app_id: person_accounts::PROVIDER.to_owned(),
            workspace: self.workspace.clone(),
            document_id: String::new(),
            browsable: true,
        }]
    }

    /// The signing in's row, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// The signing in of a mail account is the server's entry's to answer, also for a turn it
    /// is not offered to, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == person_accounts::RESOURCE
            && target.address.action == MAIL_CONNECT
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// Whether the question this server put about `ask` is in `parts` and over: told how its answer
/// ended, or taken back. A question that is not there says nothing.
fn question_over(parts: &[(String, chat::parts::ChatPart)], ask: &MailAsk) -> bool {
    let (agent, person) = (
        MemberId::agent(ask.agent.clone()),
        MemberId::person(ask.person.clone()),
    );
    let (shown, said) = (ask.detail(), ask.words());
    parts
        .iter()
        .flat_map(|(_, part)| part.messages.iter())
        .any(|(_, message)| {
            message.author == agent
                && message.text.trim() == said.trim()
                && message.question.as_ref().is_some_and(|question| {
                    question.kind.0 == chat::question::SECRET
                        && question.person == person
                        && question.kept().as_ref() == Some(&shown)
                        && match question.state() {
                            Settled(_, outcome) => outcome.outcome.0 != chat::question::STARTED,
                            Withdrawn(_) => true,
                            Waiting | Answered(_) => false,
                        }
                })
        })
}

/// An answered question about a mail account to sign in, claimed, and who asked it.
struct Carrying {
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    claimed: MailAsk,
    found: Found,
    /// The reference of what its person typed; none where they declined.
    typed: Option<String>,
    /// The agent that asked, acting for its person now: as the turn that asked, and outside
    /// any turn.
    turn: Caller,
    outside: Caller,
}

impl Carrier {
    /// Carry out what the people of `workspace` answered where they were asked for the password
    /// of a mail account to sign in: each answer once, claimed and said on its question before
    /// this returns, so what the look reads next finds no answer of these left to wake anybody
    /// for. Signing in to a mail server goes on a task of its own. Costs one look at what this
    /// server keeps when nothing waits.
    pub(super) async fn settle_mail_connects(&self, workspace: &str) {
        let open = self.workspaces.open_mail_connects(workspace);
        if open.is_empty() {
            return;
        }
        let Some(workspace_id) = WorkspaceId::parse(workspace) else {
            return;
        };
        let parts = self.chat_parts(workspace).await;
        let agents = self.agents(workspace).await.unwrap_or_default();
        for ask in open {
            if ask.started && !ask.interrupted {
                // Its answer is being carried out now.
                continue;
            }
            let Some(found) = question_about(&parts, &ask) else {
                // One this server stopped on, and one whose question is over, signs nothing in
                // any more. One whose question is not in the conversation yet is being asked.
                if ask.interrupted || (ask.put && question_over(&parts, &ask)) {
                    self.workspaces.mail_connect_not_asked(&ask);
                }
                continue;
            };
            let asker = match agents.get(&ask.agent) {
                Some(agent) => self
                    .connect_asker(workspace, workspace_id, &ask, agent)
                    .await
                    .map(|callers| (agent.clone(), callers)),
                None => None,
            };
            let Some((agent, (turn, outside))) = asker else {
                // The agent no longer works here, or its person is admitted here on no
                // device: nobody acts on the answer.
                self.workspaces.mail_connect_not_asked(&ask);
                continue;
            };
            let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
            let at = (found.document.as_str(), found.message.as_str());
            if ask.interrupted {
                // This server stopped while it signed the account in: it is not signed in a
                // second time, and its person is told.
                self.workspaces.mail_connect_not_asked(&ask);
                let told = Some(INTERRUPTED.to_owned());
                let failed = chat::question::FAILED;
                if let Err(error) = settle((&service, &outside), workspace, at, failed, told).await
                {
                    tracing::warn!(%workspace, %error, "an interrupted mail account's question did not take how it ended");
                }
                continue;
            }
            let typed = match &found.stands {
                Stands::Waiting => continue,
                Stands::Answered(typed) => typed.clone(),
                // Said to have started, with nothing claimed here: no answer of it is left.
                Stands::Started => {
                    self.workspaces.mail_connect_not_asked(&ask);
                    continue;
                }
            };
            // Claimed before anything is said or done. Claimed by another look already, it is
            // that one's.
            let Ok(claimed) = self.workspaces.claim_mail_connect(&ask.ask) else {
                continue;
            };
            // Only what they typed under the question as it was asked signs in anywhere: that
            // takes a mail server's time, so it is said to have started here and carried out on
            // a task of its own.
            let signs_in = typed.is_some() && found.as_asked;
            if signs_in
                && let Err(error) = settle(
                    (&service, &outside),
                    workspace,
                    at,
                    chat::question::STARTED,
                    None,
                )
                .await
            {
                // It cannot be said to have started (the question was taken back meanwhile):
                // nothing is signed in, and the ask is spent.
                tracing::warn!(%workspace, %error, "a mail account's question did not take that it started");
                self.workspaces.mail_connect_not_asked(&claimed);
                continue;
            }
            let carrying = Carrying {
                workspace: workspace.to_owned(),
                workspace_id,
                agent,
                claimed,
                found,
                typed,
                turn,
                outside,
            };
            if signs_in {
                let this = self.clone();
                tokio::spawn(async move {
                    this.carry_out_mail_connect(carrying).await;
                });
            } else {
                // Declined, or typed under a question that is not the server's: no mail server
                // is asked anything, and how it ended is said before this returns.
                self.carry_out_mail_connect(carrying).await;
            }
        }
    }

    /// Carry `carrying` out: let the entry do what it kept for the ask, say on the question how
    /// it ended, and tell the agent that asked, once. Where a mail server did not take the
    /// password its person is asked again instead, and no agent is told anything.
    async fn carry_out_mail_connect(&self, carrying: Carrying) {
        let Carrying {
            workspace,
            workspace_id,
            agent,
            claimed,
            found,
            typed,
            turn,
            outside,
        } = carrying;
        let narrowed = narrowing(&agent, &workspace);
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &turn,
            keeper: &agent.keeper,
            bound: narrowed.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_mail_connect(&hosted, &claimed, typed.as_deref(), found.as_asked)
            .await;
        let service = WorkspaceActionProvider::new(self.actions.clone(), workspace_id);
        let at = (found.document.as_str(), found.message.as_str());
        let (outcome, told, answered, came) = match carried {
            Ok(MailCarried::SignedIn(signed_in)) => (
                chat::question::DONE,
                Some(format!(
                    "Signed in. Its mail is read at {} and sent through {}.",
                    signed_in.reads, signed_in.sends
                )),
                TYPED,
                format!(
                    "The mail account {} is signed in on your person's server: its mail is read \
                     at {} and sent through {}. You are named for it and read, search and draft \
                     its mail with the mail actions. Tell them in one line that it is signed in.",
                    signed_in.address, signed_in.reads, signed_in.sends
                ),
            ),
            Ok(MailCarried::Declined) => (
                chat::question::DROPPED,
                None,
                DECLINED,
                "Nothing was signed in.".to_owned(),
            ),
            Ok(MailCarried::AsksAgain(again)) => {
                let told = format!(
                    "That password was not accepted by {}. Nothing of it is kept. I ask for it \
                     again below.",
                    again.refused_by
                );
                let failed = chat::question::FAILED;
                if let Err(error) =
                    settle((&service, &outside), &workspace, at, failed, Some(told)).await
                {
                    tracing::warn!(%workspace, %error, "a mail account's question did not take that its password was refused");
                }
                match put(&service, &turn, &workspace, again.as_ref()).await {
                    Ok(()) => self.workspaces.mail_connect_put(&again),
                    Err(why) => {
                        self.workspaces.mail_connect_not_asked(&again);
                        let came = format!(
                            "{} did not accept that password, and they could not be asked for \
                             it again ({why}). Nothing was kept, and what they typed is not kept \
                             either. Tell them so; they tell you again when they want to retry.",
                            again.refused_by
                        );
                        self.tell_of_mail(&workspace, &found, &claimed, (TYPED, &came))
                            .await;
                    }
                }
                return;
            }
            Err(why) => (
                chat::question::FAILED,
                Some(format!(
                    "Your mail account was not signed in: {why}. What you typed is not kept."
                )),
                TYPED,
                format!(
                    "It was not signed in: {why}. Nothing was kept, and what they typed is not \
                     kept either. Tell them so; they tell you again when they want to retry."
                ),
            ),
        };
        if let Err(error) = settle((&service, &outside), &workspace, at, outcome, told).await {
            tracing::warn!(%workspace, %error, "a mail account's question did not take how it ended");
        }
        self.tell_of_mail(&workspace, &found, &claimed, (answered, &came))
            .await;
    }

    /// Wake the agent that asked `claimed` once with how its person answered and what came of
    /// it.
    async fn tell_of_mail(
        &self,
        workspace: &str,
        found: &Found,
        claimed: &MailAsk,
        (answered, came): (&str, &str),
    ) {
        let news = super::answers::carried_out(
            (&found.message, &found.conversation_id),
            &claimed.person,
            &claimed.named(),
            (answered, came),
        );
        if let Err(error) = self.wake(workspace, &news).await {
            tracing::warn!(%workspace, %error, "an agent was not told how signing a mail account in ended");
        }
    }
}
