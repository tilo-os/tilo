//! A mail account its person tells their assistant of by its address, signed in on this server.
//!
//! The same asking a calendar is connected by ([`super::calendar_connect`]), for mail. A turn
//! hands over an address and nothing that is a secret; the server's entry
//! ([`crate::workspaces::Workspaces::connect_mail_for_turn`]) works out the account's two
//! servers and keeps what was asked. [`TurnMailConnects`] is that entry as one turn's tools
//! reach it, a provider of the turn's own ([`TurnProvider`]): it lists the one action beside the
//! workspace's resources, and takes it. The tool call ends at once, told that its person is
//! asked and which servers were found, and this server asks them on their own devices, in the
//! system's protected password field, in the words the entry says, which name both servers.
//! What they type goes into the workspace's vault.
//!
//! A password a mail server does not take is asked for again by the server itself, saying which
//! server refused it, and no model is asked anything for that. Once the account is signed in, or
//! where signing in failed for another reason, the agent that asked is woken once with what came
//! of it. Neither the password nor the item's reference passes a model on this way.

use std::sync::Arc;

use serde_json::Value;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_workspace_agent::Agent;

use super::ask_chat::asked_in;
use super::calendar_connect::{
    ASKED_FIRST, Asker, Asking, Carrier, DECLINED, PutAsk, TYPED, Typed, UNANSWERED,
    ask_for_password,
};
use super::turn_provider::{Ending, Rows};
use super::{Reply, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::hosted_use::{
    AskedSigner, HostedTurn, MAIL_CONNECT, MailAsk, MailCarried, TurnMailConnect,
};
use tilo_actions::remote::person_vault::AskForItem;

/// What an agent is told when its person is asked for the password: nothing is signed in yet,
/// and it neither waits nor asks again.
const ASKED: &str = "Your person is asked on their own device for the password of this mail \
     account, in a protected field you cannot read. Nothing is signed in yet. Tell them in one \
     line to type it there. Once they have, this server signs the account in and you are told; \
     a password that is not accepted is asked for again by this server itself. Do not make this \
     call again.";

/// What an agent is told when its person was asked for this account's password before and has
/// not answered.
const WAITS: &str = "Your person was asked for the password of this mail account before, in a \
     protected field on their own device, and has not typed it yet. Nothing is signed in yet. \
     Tell them it waits for them there, and do not make this call again.";

/// What the agent is told when this server stopped while it signed the account in.
const INTERRUPTED: &str = "This server restarted while the account was being signed in, so it \
     may not be. Tell them so; they tell you again if it is still wanted.";

impl PutAsk for MailAsk {
    fn grant(&self) -> (Option<&AskedSigner>, &'static str) {
        (self.signer.as_ref(), tilo_app_id::apps::MAIL)
    }

    fn id(&self) -> &str {
        &self.ask
    }

    fn by_agent(&self) -> &str {
        &self.agent
    }

    fn agent_name(&self) -> &str {
        &self.name
    }

    fn of_person(&self) -> &str {
        &self.person
    }

    fn in_workspace(&self) -> &str {
        &self.workspace
    }

    fn item(&self) -> AskForItem {
        MailAsk::login(self)
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
    carrier: Carrier,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The signing in this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnMailConnects {
    /// What `turn` signs in of its person's mail, where `reply` holds what its tools begin as
    /// and act through.
    pub(super) async fn for_turn(carrier: &Carrier, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            bound: narrowed.as_deref(),
        };
        let offered = carrier.workspaces.mail_connect_offered(&hosted).await;
        Arc::new(Self {
            carrier: carrier.clone(),
            workspace: reply.workspace.clone(),
            workspace_id,
            agent: turn.agent.clone(),
            offered,
        })
    }

    /// Take the telling `request` makes as `caller`: its person is asked, or the server's
    /// refusal.
    async fn take(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<InvocationUpdate, ActionCallError> {
        let narrowed = self.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: self.workspace_id,
            caller,
            bound: narrowed.as_deref(),
        };
        let taken = self
            .carrier
            .workspaces
            .connect_mail_for_turn(&hosted, request, asked_in(caller))
            .await?;
        let words = match taken {
            TurnMailConnect::Waits => WAITS.to_owned(),
            TurnMailConnect::Asks(open) => {
                let servers = open.servers();
                let carrier = self.carrier.clone();
                tokio::spawn(async move { carrier.ask_and_sign_in(*open).await });
                format!("{ASKED} What this server worked out from the address: {servers}.")
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

impl Carrier {
    /// Ask again about every mail account of `workspace` whose person this process does not
    /// ask about, as after a restart, and tell the agent of one this server stopped on while it
    /// signed it in. Costs one look at what this server keeps when nothing waits.
    pub(super) fn settle_mail_connects(&self, workspace: &str) {
        for ask in self.workspaces.open_mail_connects(workspace) {
            if ask.interrupted {
                self.workspaces.mail_connect_not_asked(&ask);
                let this = self.clone();
                tokio::spawn(async move {
                    this.tell_of_password(&ask, &ask.named(), (TYPED, INTERRUPTED))
                        .await;
                });
            } else if !ask.started {
                let this = self.clone();
                tokio::spawn(async move { this.ask_and_sign_in(ask).await });
            }
        }
    }

    /// Ask the person `open` names for the password, sign the account in with what they typed,
    /// and tell the agent that asked, once. Where a mail server does not take the password its
    /// person is asked again instead.
    async fn ask_and_sign_in(&self, open: MailAsk) {
        let mut open = open;
        loop {
            let Some(_asking) = Asking::hold(&open.ask) else {
                return;
            };
            let Some(asker) = self.connect_asker(&open).await else {
                // The agent works here no more, or its person is admitted here on no device:
                // nobody would act on the answer.
                self.workspaces.mail_connect_not_asked(&open);
                return;
            };
            let typed = match ask_for_password(&self.workspaces, &open).await {
                Typed::Kept(kept) => Some(kept.to_string()),
                Typed::Declined => None,
                Typed::Unanswered(why) => {
                    self.workspaces.mail_connect_not_asked(&open);
                    let came = format!(
                        "Nothing was signed in: {why}. Tell them so; they tell you again when \
                         they want to retry."
                    );
                    self.tell_of_password(&open, &open.named(), (UNANSWERED, &came))
                        .await;
                    return;
                }
            };
            // Claimed before anything is done. Claimed by another already, it is that one's.
            let Ok(claimed) = self.workspaces.claim_mail_connect(&open.ask) else {
                return;
            };
            match self.carry_out_mail_connect(&asker, &claimed, typed).await {
                Some(again) => open = again,
                None => return,
            }
        }
    }

    /// Let the entry do what it kept for `claimed` with the reference its person's password is
    /// kept under (`None` where they declined), and tell the agent that asked, once. Where a
    /// mail server did not take the password, the ask to put to its person again instead.
    async fn carry_out_mail_connect(
        &self,
        asker: &Asker,
        claimed: &MailAsk,
        typed: Option<String>,
    ) -> Option<MailAsk> {
        let narrowed = asker.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: asker.workspace_id,
            caller: &asker.turn,
            bound: narrowed.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_mail_connect(&hosted, claimed, typed.as_deref())
            .await;
        let (answered, came) = match carried {
            Ok(MailCarried::SignedIn(signed_in)) => (
                TYPED,
                format!(
                    "The mail account {} is signed in for the workspace: its mail is read \
                     at {} and sent through {}. You read, search and draft \
                     its mail with the mail actions. Tell them in one line that it is signed in.",
                    signed_in.address, signed_in.reads, signed_in.sends
                ),
            ),
            Ok(MailCarried::Declined) => (DECLINED, "Nothing was signed in.".to_owned()),
            Ok(MailCarried::AsksAgain(again)) => return Some(*again),
            Err(why) => (
                TYPED,
                format!(
                    "It was not signed in: {why}. Nothing was kept, and what they typed is not \
                     kept either. Tell them so; they tell you again when they want to retry."
                ),
            ),
        };
        self.tell_of_password(claimed, &claimed.named(), (answered, &came))
            .await;
        None
    }
}
