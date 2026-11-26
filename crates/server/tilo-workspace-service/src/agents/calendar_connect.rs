//! A calendar its person tells their assistant of, connected on this server.
//!
//! An agent's tools act on the workspace's action service, which knows the workspace's
//! resources and nothing of a person's own calendars. Connecting one is the server's own to
//! answer, through its entry for a turn it carries out
//! ([`crate::workspaces::Workspaces::connect_calendar_for_turn`]): the turn is one this server carries out. [`TurnConnects`] is that entry as one turn's tools reach it, a provider of the turn's
//! own ([`TurnProvider`]): it lists the one action beside the workspace's resources, and takes
//! it. The action takes what the person said (the calendar server's address, the user name
//! there, a name for the account) and nothing else.
//!
//! The password is its person's to type where no agent reads it. So the tool call ends at once,
//! told that they are asked, and this server asks them itself, on their own devices: the
//! system's protected password field on the sheet their shell or their page draws, in the words
//! the server's entry says ([`Workspaces::ask_for_item`]). What they type goes from that field
//! to this server and into the workspace's vault; only the item's reference comes back here.
//!
//! The ask is claimed once they answered, so an answer is carried out at most once. Then the
//! entry signs in to the calendar server, keeps the account in the workspace for reading only, and
//! the agent is woken once, in its conversation with its person, with what came of
//! it: the calendars by their names, or the calendar server's refusal. Neither the password nor
//! the item's reference passes a model on this way. A server that restarts while its person is
//! asked asks them again.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_actions::policy::MemberId;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{
    ActionCallError, Caller, CatalogueProvider, CatalogueQuery, CatalogueRow, InvokeRequest,
    Pending, RemoteActionTarget, person_accounts,
};
use tilo_actions::{ActionFailure, InvocationUpdate};
use tilo_lock::MutexRecover as _;
use tilo_server_protocol::certify::{AgentAsk, PendingConfirmation};
use tilo_workspace_agent::Agent;

use super::ask_chat::{AskCallers, asked_in};
use super::turn_provider::{Ending, Rows};
use super::{Reply, Turn, TurnProvider, WorkspaceId};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{
    AskedSigner, CALENDAR_CONNECT, CALENDARS, ConnectAsk, Connected, HostedTurn, TurnConnect,
};
use tilo_actions::remote::person_vault::AskForItem;

/// What an agent is told when its person is asked for the password: nothing is connected yet,
/// and it neither waits nor asks again.
const ASKED: &str = "Your person is asked on their own device for the password of this \
     calendar account, in a protected field you cannot read. Nothing is connected yet. Tell them \
     in one line to type it there. Once they have, the account is connected for reading only and \
     you are told which calendars it has: do not make this call again.";

/// What an agent is told when its person was asked for this account's password before and has
/// not answered.
const WAITS: &str = "Your person was asked for the password of this calendar account before, in \
     a protected field on their own device, and has not typed it yet. Nothing is connected yet. \
     Tell them it waits for them there, and do not make this call again.";

/// The failure code of a call that connected nothing because its person is asked first. It is
/// not the code of an action that wants a confirmation: the agent's tools word that one
/// themselves, and the words here are the ones the agent must read.
pub(super) const ASKED_FIRST: &str = "asked_person_first";

/// How long its person's devices ask for a password before the ask lapses, in nanoseconds.
const PASSWORD_WAIT_NS: u64 = 30 * 60 * 1_000_000_000;

/// What the agent is told when this server stopped while it connected the calendar.
const INTERRUPTED: &str = "This server restarted while the account was being connected, so it \
     may not be. Tell them so; they tell you again if it is still wanted.";

/// What the agent is told its person did, where they typed, and where they declined.
pub(super) const TYPED: &str = "they typed the password where you cannot read it";
pub(super) const DECLINED: &str = "they declined to type the password";
/// What the agent is told where nobody could be asked, or nobody answered in time.
pub(super) const UNANSWERED: &str = "they were not asked, or did not answer in time";

/// The most of the calendars' names a question's outcome and an agent's news carry, in bytes.
const MOST_NAMES_BYTES: usize = 600;

/// Whoever carries out a workspace's agents: what reads the answers to the questions this
/// server put itself, and carries them out.
pub(super) type Carrier = super::ServerAgents;

/// What this server keeps of an ask for a person's password: enough to ask it, and to tell the
/// agent that asked what came of it.
pub(super) trait PutAsk: Sync {
    /// The signing session and app whose access made this question possible.
    fn grant(&self) -> (Option<&AskedSigner>, &'static str);
    /// The ask's own id.
    fn id(&self) -> &str;
    /// The agent that asked, by its id.
    fn by_agent(&self) -> &str;
    /// The name its person knows that agent under.
    fn agent_name(&self) -> &str;
    /// The person asked: their root key, hex.
    fn of_person(&self) -> &str;
    /// The workspace it was asked in.
    fn in_workspace(&self) -> &str;
    /// What the vault keeps what is typed as.
    fn item(&self) -> AskForItem;
    /// The question's words, as this server says them.
    fn said(&self) -> String;
    /// Where and for which turn it was asked.
    fn put_in(&self) -> &Value;
}

impl PutAsk for ConnectAsk {
    fn grant(&self) -> (Option<&AskedSigner>, &'static str) {
        (self.signer.as_ref(), tilo_app_id::apps::CALENDAR)
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
        ConnectAsk::login(self)
    }

    fn said(&self) -> String {
        self.words()
    }

    fn put_in(&self) -> &Value {
        &self.asked_in
    }
}

/// The asks whose person is being asked by this process now: one asked again after a restart is
/// not among them.
static ASKING: Mutex<BTreeSet<String>> = Mutex::new(BTreeSet::new());

/// An ask this process asks about while it is held: let go of once the answer is carried out.
pub(super) struct Asking(String);

impl Asking {
    /// Hold `ask` while its person is asked; `None` where this process asks about it already.
    pub(super) fn hold(ask: &str) -> Option<Self> {
        ASKING
            .lock_recover()
            .insert(ask.to_owned())
            .then(|| Self(ask.to_owned()))
    }
}

impl Drop for Asking {
    fn drop(&mut self) {
        ASKING.lock_recover().remove(&self.0);
    }
}

/// Where the agent that asked `open` is told what came of it: its conversation with its person,
/// under the ask's own key.
pub(super) struct Told {
    pub(super) key: String,
    pub(super) conversation_id: String,
}

impl Told {
    pub(super) fn of(open: &dyn PutAsk) -> Self {
        Self {
            key: open.id().to_owned(),
            conversation_id: chat::Chat::direct_id([
                MemberId::person(open.of_person().to_owned()),
                MemberId::agent(open.by_agent().to_owned()),
            ])
            .to_string(),
        }
    }
}

/// How its person answered where they were asked for a password.
pub(super) enum Typed {
    /// They typed it, and the vault keeps it under this reference.
    Kept(ItemRef),
    /// They declined.
    Declined,
    /// Nobody could be asked, or nobody answered in time.
    Unanswered(String),
}

/// The person to ask or answer while the original signing session still has access.
async fn permitted_person(workspaces: &Workspaces, open: &dyn PutAsk) -> Option<[u8; 32]> {
    let mut person = [0; 32];
    hex::decode_to_slice(open.of_person(), &mut person).ok()?;
    let (signer, app) = open.grant();
    AskedSigner::permits(
        signer,
        workspaces,
        open.in_workspace(),
        open.by_agent(),
        &person,
        app,
    )
    .await
    .then_some(person)
}

/// Ask the person `open` names for its password on their own devices, in this server's words,
/// and keep what they type in the workspace's vault. Waits for their answer.
pub(super) async fn ask_for_password(workspaces: &Workspaces, open: &dyn PutAsk) -> Typed {
    let Some(person) = permitted_person(workspaces, open).await else {
        return Typed::Unanswered(
            "This assistant no longer has access for this request.".to_owned(),
        );
    };
    let workspace = match workspaces.summary(open.in_workspace()).await {
        Ok(summary) => summary.name,
        Err(_) => open.in_workspace().to_owned(),
    };
    let question = PendingConfirmation {
        id: *blake3::hash(format!("password ask\0{}", open.id()).as_bytes()).as_bytes(),
        agent_id: open.by_agent().to_owned(),
        agent_label: open.agent_name().to_owned(),
        workspace,
        expires_at_ns: tilo_clock::unix_now_ns().saturating_add(PASSWORD_WAIT_NS),
        ask: Some(AgentAsk {
            question: open.said(),
            choices: Vec::new(),
            secret: true,
        }),
        ..PendingConfirmation::default()
    };
    match workspaces
        .ask_for_item((person, open.in_workspace()), question, open.item())
        .await
    {
        Ok(Some(kept)) => Typed::Kept(kept),
        Ok(None) => Typed::Declined,
        Err(why) => Typed::Unanswered(why),
    }
}

/// What one turn connects of its person's calendars. A turn's caller changes when its person
/// writes again while it works, so each telling is taken as the caller of the tool call that
/// made it.
pub(super) struct TurnConnects {
    carrier: Carrier,
    workspace: String,
    workspace_id: WorkspaceId,
    agent: Agent,
    /// The connecting this turn is offered: none for a turn the entry refuses.
    offered: Vec<CatalogueRow>,
}

impl TurnConnects {
    /// What `turn` connects of its person's calendars, where `reply` holds what its tools begin
    /// as and act through.
    pub(super) async fn for_turn(carrier: &Carrier, turn: &Turn, reply: &Reply) -> Arc<Self> {
        let workspace_id = reply.provider.workspace;
        let narrowed = turn.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: workspace_id,
            caller: &reply.caller,
            bound: narrowed.as_deref(),
        };
        let offered = carrier.workspaces.calendar_connect_offered(&hosted).await;
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
            .connect_calendar_for_turn(&hosted, request, asked_in(caller))
            .await?;
        let words = match taken {
            TurnConnect::Waits => WAITS,
            TurnConnect::Asks(open) => {
                let carrier = self.carrier.clone();
                tokio::spawn(async move { carrier.ask_and_connect(*open).await });
                ASKED
            }
        };
        Ok(InvocationUpdate::Failed {
            invocation: request.invocation,
            error: ActionFailure {
                code: ASKED_FIRST.to_owned(),
                message: words.to_owned(),
                details: None,
                retryable: false,
            },
        })
    }
}

impl TurnProvider for TurnConnects {
    /// The provider of a person's accounts, as a listing of everything names it beside the
    /// workspace's resources, where this turn is offered the connecting.
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

    /// The connecting's row, when `query` asks the provider of a person's accounts.
    fn rows(&self, _caller: &Caller, query: &CatalogueQuery) -> Option<Rows> {
        (query.provider.as_deref() == Some(person_accounts::PROVIDER) && !self.offered.is_empty())
            .then(|| Pending::ready(Ok(self.offered.clone())))
    }

    /// The connecting of a calendar is the server's entry's to answer, also for a turn it is
    /// not offered to, which the entry then refuses.
    fn takes(&self, target: &RemoteActionTarget) -> bool {
        target.provider == person_accounts::PROVIDER
            && target.address.resource == CALENDARS
            && target.address.action == CALENDAR_CONNECT
    }

    fn invoke(self: Arc<Self>, caller: Caller, request: InvokeRequest) -> Ending {
        Box::pin(async move { self.take(&caller, &request).await })
    }
}

/// The calendars of `connected` by their names, as many as fit: `none` where the server lists
/// none.
fn names_of(connected: &Connected, none: &str) -> String {
    let mut names = String::new();
    let mut left = connected.calendars.len();
    for name in &connected.calendars {
        if names.len().saturating_add(name.len()) > MOST_NAMES_BYTES {
            break;
        }
        if !names.is_empty() {
            names.push_str(", ");
        }
        names.push('"');
        names.push_str(name);
        names.push('"');
        left = left.saturating_sub(1);
    }
    match (names.is_empty(), left) {
        (true, 0) => none.to_owned(),
        (true, _) => format!("{left} calendars"),
        (false, 0) => names,
        (false, _) => format!("{names} and {left} more"),
    }
}

/// The agent that asked, acting for its person now as the turn that asked.
pub(super) struct Asker {
    pub(super) workspace_id: WorkspaceId,
    pub(super) agent: Agent,
    pub(super) turn: Caller,
}

impl Carrier {
    /// Ask again about every ask of `workspace` this process does not ask about, as after a
    /// restart, and tell the agent of one this server stopped on while it was carried out. Costs
    /// one look at what this server keeps when nothing waits.
    pub(super) fn settle_connects(&self, workspace: &str) {
        for ask in self.workspaces.open_connects(workspace) {
            if ask.interrupted {
                self.workspaces.connect_not_asked(&ask);
                let this = self.clone();
                tokio::spawn(async move {
                    this.tell_of_password(&ask, &ask.named(), (TYPED, INTERRUPTED))
                        .await;
                });
            } else if !ask.started {
                let this = self.clone();
                tokio::spawn(async move { this.ask_and_connect(ask).await });
            }
        }
    }

    /// Ask the person `open` names for the password, then connect the calendar with what they
    /// typed and tell the agent that asked, once.
    async fn ask_and_connect(&self, open: ConnectAsk) {
        let Some(_asking) = Asking::hold(&open.ask) else {
            return;
        };
        let Some(asker) = self.connect_asker(&open).await else {
            // The agent works here no more, or its person is admitted here on no device: nobody
            // would act on the answer.
            self.workspaces.connect_not_asked(&open);
            return;
        };
        let typed = match ask_for_password(&self.workspaces, &open).await {
            Typed::Kept(kept) => Some(kept.to_string()),
            Typed::Declined => None,
            Typed::Unanswered(why) => {
                self.workspaces.connect_not_asked(&open);
                let came = format!(
                    "Nothing was connected: {why}. Tell them so; they tell you again when they \
                     want to retry."
                );
                self.tell_of_password(&open, &open.named(), (UNANSWERED, &came))
                    .await;
                return;
            }
        };
        // Claimed before anything is done. Claimed by another already, it is that one's.
        let Ok(claimed) = self.workspaces.claim_connect(&open.ask) else {
            return;
        };
        self.carry_out_connect(&asker, claimed, typed).await;
    }

    /// The agent that asked `ask`, working for its person now: as the turn that asked, which is
    /// what the entry decides it by. `None` when the agent works here no more or has no approved
    /// key here, or the workspace admits the person on no device.
    pub(super) async fn connect_asker(&self, ask: &dyn PutAsk) -> Option<Asker> {
        let workspace = ask.in_workspace();
        let workspace_id = WorkspaceId::parse(workspace)?;
        let agent = self.agents(workspace).await.ok()?.remove(ask.by_agent())?;
        let AskCallers { turn, .. } = self
            .ask_callers(workspace_id, &agent, ask.of_person(), ask.put_in())
            .await?;
        Some(Asker {
            workspace_id,
            agent,
            turn,
        })
    }

    /// Wake the agent that asked `open` once with how its person answered and what came of it.
    pub(super) async fn tell_of_password(
        &self,
        open: &dyn PutAsk,
        asked: &str,
        (answered, came): (&str, &str),
    ) {
        if permitted_person(&self.workspaces, open).await.is_none() {
            return;
        }
        let told = Told::of(open);
        let news = super::answers::carried_out(
            (&told.key, &told.conversation_id),
            open.of_person(),
            asked,
            (answered, came),
        );
        let workspace = open.in_workspace();
        if let Err(error) = self.wake(workspace, &news).await {
            tracing::warn!(%workspace, %error, "an agent was not told how asking for a password ended");
        }
    }

    /// Let the entry do what it kept for `claimed` with the reference its person's password is
    /// kept under (`None` where they declined), and tell the agent that asked, once.
    async fn carry_out_connect(&self, asker: &Asker, claimed: ConnectAsk, typed: Option<String>) {
        let narrowed = asker.agent.bound_rules();
        let hosted = HostedTurn {
            workspace: asker.workspace_id,
            caller: &asker.turn,
            bound: narrowed.as_deref(),
        };
        let carried = self
            .workspaces
            .carry_out_connect(&hosted, &claimed, typed.as_deref())
            .await;
        let (answered, came) = match &carried {
            Ok(Some(connected)) => (
                TYPED,
                format!(
                    "The account is connected for reading only: nothing is ever written to that \
                     calendar server. The workspace keeps it, and you read it with the \
                     calendar actions. Its calendars, as the calendar server names them: {}.",
                    names_of(connected, "none yet")
                ),
            ),
            Ok(None) => (DECLINED, "Nothing was connected.".to_owned()),
            Err(why) => (
                TYPED,
                format!(
                    "It was not connected: {why}. Nothing was kept, and what they typed is not \
                     kept either. Tell them so; they tell you again when they want to retry."
                ),
            ),
        };
        self.tell_of_password(&claimed, &claimed.named(), (answered, &came))
            .await;
    }
}
