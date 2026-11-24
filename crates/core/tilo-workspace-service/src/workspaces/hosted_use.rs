//! A secret used by reference by a turn this server carries out itself.
//!
//! An agent this server runs works with every device of whoever asked it off, so no transport
//! verified a device behind what it asks, and the check a device's request passes
//! ([`Workspaces::reaching`]) has nothing to compare. This is the device's own entry for such a
//! turn. The turn's tools reach it as every other action, through the device's dispatch
//! ([`answers`], [`Workspaces::answer_turn`]), with the very caller they act as; what narrows
//! the agent is what the workspace's list of its agents keeps for it ([`HostedTurn`]).
//!
//! The rule is the vault's own, and nothing here hands a secret to whoever
//! asks. A reference becomes its secret only inside this server, only in the
//! vault of the workspace the turn works in, when the workspace's policy
//! lets the person the turn acts for use it, only for the kind the use needs
//! and only at the places the use sends it to, with the use written down
//! first ([`accounts::vault_items::VaultItems::use_as`]). The caller is
//! answered what the person would be: the account, the provider.
//!
//! Before the vault is opened a turn is refused unless it is one this server
//! carries out ([`Workspaces::asked_turn`]):
//!
//! - the caller is an agent a person's session was handed to, working
//!   inside a turn, confined to this workspace. A bot, an app, an agent
//!   admitted as a member and a person asking for themselves are not turns
//!   this server carries out;
//! - the workspace admits that person now, on the device the caller names:
//!   a person removed from it leaves the turn they asked nothing.
//!
//! Any member may ask any of the workspace's agents, and what the turn then
//! reaches of the workspace's vault, mail, calendars and connections is what
//! the workspace's policy decides for that agent acting on that ask
//! ([`Workspaces::turn_reaches`]): the same decision every record of the
//! workspace takes, so the agent's own grants, and an owner's word on them,
//! hold whoever asked. The person who asked is who the turn's History names
//! and who a question about a use goes to. Here, an agent that anything
//! narrows (what its entry bounds it to; a sub-agent) must hold a rule that
//! names exactly this use ([`HostedTurn::may_take`]), and nothing is used
//! unasked.
//!
//! A turn the server carries out is unattended. So a use is made unasked
//! only where workspace policy keeps a grant for this agent, this use and
//! these places. Otherwise the call waits for the person's yes, shown the
//! item by its kind and the places its secret would go: the dispatch keeps it
//! as its asked record, and their approve admits the same call again, held
//! to every refusal here once more.
//!
//! Another use by reference (a key added to a web call, a variable handed
//! to a command) is one more arm of [`Use`]: what it wants of the vault and
//! how it is carried out. The refusals, the asking and the leave are the
//! same for every use.
//!
//! A page's field is the one use whose secret is not used inside this
//! server: the page is in a window, and only its engine writes into it. So
//! the text is released to whoever sends it on to that window
//! ([`Workspaces::release_for_turn`]), for the one site the window's engine
//! says its page is at, through the same refusals and the same asking.

use std::path::Path;

use accounts::vault_items::{self, Allowance, ItemId, Place, UsedFor, VaultItems};
use serde_json::{Map, Value, json};
use tilo_action_policy::{ActionKind, Answer, Context, Grantee, Party, TreePath, WorkspacePolicy};
use tilo_actions::remote::ActionCallError::{self, InvalidRequest, NotPermitted, UnknownAction};
#[cfg(feature = "agents")]
use tilo_actions::remote::CatalogueRow;
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::model_providers::{self, SetKeyFromItem};
use tilo_actions::remote::person_accounts::{self, AddMailWithItem};
#[cfg(feature = "agents")]
use tilo_actions::remote::person_vault::FillPage;
use tilo_actions::remote::person_vault::{self, ItemRef};
use tilo_actions::remote::{
    Caller, InvokeArgument, InvokeRequest, RemoteActionTarget, person_connections,
};

use super::workspace_stores::Store;
use super::{WorkspaceId, Workspaces};

mod web_call;

/// The workspace's mail as such a turn reaches it: after [`Workspaces::asked_turn`], the
/// workspace's word on which agent may use which account ([`Workspaces::mail_for_turn`]).
pub use super::person_settings::turn_mail::AskedSend;

/// A person's calendars as such a turn reads them ([`Workspaces::calendars_for_turn`]): the
/// resource they are asked under, and the read whose answer holds what other people wrote.
#[cfg(feature = "agents")]
pub use super::person_settings::calendars::{ENTRIES as CALENDAR_ENTRIES, RESOURCE as CALENDARS};

/// A person's calendar as such a turn connects it, by their telling
/// ([`Workspaces::connect_calendar_for_turn`]): the action, what is kept while its person is
/// asked for the password, and what comes of their answer.
#[cfg(feature = "agents")]
pub use super::person_settings::calendars::CONNECT as CALENDAR_CONNECT;
#[cfg(feature = "agents")]
pub use super::person_settings::calendars::connect::Connected;

/// A person's mail account as such a turn signs it in from its address alone
/// ([`Workspaces::connect_mail_for_turn`]): the action, what is kept while its person is
/// asked for the password, what comes of their answer, and where an address's mail lives
/// as whatever works that out answers it.
#[cfg(feature = "agents")]
pub use super::person_settings::mail_connect::{CONNECT as MAIL_CONNECT, MailSignedIn};
#[cfg(feature = "agents")]
pub use tilo_account_probe::mail_servers::{Found as MailFound, MailServers, Server as MailServer};
/// A mail server with real sockets and made-up mail, for a test to sign in to and read from.
#[cfg(any(test, feature = "test-support"))]
pub use tilo_account_probe::stand_in as mail_stand_in;

/// A turn this server carries out for the person who asked it, as the code that carries it
/// out knows it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct HostedTurn<'a> {
    /// The workspace the turn works in.
    pub(crate) workspace: WorkspaceId,
    /// The caller the turn's tools act as: the asking person's session,
    /// handed to the agent on this turn.
    pub(crate) caller: &'a Caller,
    /// The workspace's policy, where the agent's own answers decide what it may take:
    /// `None` for a turn nothing narrows ([`Workspaces::asked_turn_of`]).
    pub(crate) policy: Option<&'a WorkspacePolicy>,
}

impl HostedTurn<'_> {
    /// Whether the workspace's answer for the turn's agent lets it take `target` in
    /// `workspace`: one nothing narrows may, and a narrowed one only where the workspace's
    /// grants on the agent allow this very action.
    #[must_use]
    pub(crate) fn may_take(&self, workspace: &str, target: &RemoteActionTarget) -> bool {
        agent_may_take(self.policy, self.caller, workspace, target)
    }
}

/// Whether the workspace's policy lets the agent `caller` acts as take `target` in `workspace`:
/// an Allow of the workspace's on that very action, and nothing where no policy narrows it.
#[must_use]
pub fn agent_may_take(
    policy: Option<&WorkspacePolicy>,
    caller: &Caller,
    workspace: &str,
    target: &RemoteActionTarget,
) -> bool {
    agent_policy_allows(policy, caller.acting().agent_id(), workspace, target)
}

/// Whether the workspace's policy lets `agent` take `target` in `workspace`: an Allow of the
/// workspace's on that very action, and nothing where it keeps no policy.
#[must_use]
pub fn agent_policy_allows(
    policy: Option<&WorkspacePolicy>,
    agent: Option<&str>,
    workspace: &str,
    target: &RemoteActionTarget,
) -> bool {
    let Some(policy) = policy else {
        return true;
    };
    let Some(agent) = agent else {
        return false;
    };
    let node = TreePath::workspace(workspace)
        .app(target.provider.clone())
        .kind(target.address.resource.clone())
        .resource(target.address.resource.clone())
        .action(target.address.action.clone());
    Party::new(vec![Grantee::agent(agent.to_owned())], &policy.grants)
        .decide(
            &node,
            ActionKind::Outside,
            &Context::at_time(tilo_clock::wall()),
        )
        .answer
        == Answer::Allow
}

/// What an agent's turn asks of what its workspace keeps for the person who asked it, as the
/// device's dispatch answers it ([`Workspaces::answer_turn`]).
#[derive(Clone, Copy)]
enum TurnCall {
    /// A use of their vault by reference: a key added to a web call, a sign-in kept from an item.
    Use,
    /// An ask for a secret their vault does not hold yet, which they type into it.
    AskForSecret,
    /// Their mail.
    Mail,
    /// A mail account signed in from the address they told it.
    #[cfg(feature = "agents")]
    MailConnect,
    /// A read of their calendars.
    #[cfg(feature = "agents")]
    Calendars,
    /// A calendar connected from what they told it.
    #[cfg(feature = "agents")]
    CalendarConnect,
    /// A call of a service they connected.
    Connections,
}

impl TurnCall {
    /// What a call of `target` asks, where it is one of these. Filling a page's field is
    /// carried out where the turn runs, as only the window's engine writes into its page; what
    /// may be filled is decided here ([`Workspaces::release_for_turn`]).
    fn of(target: &RemoteActionTarget) -> Option<Self> {
        if *target == person_vault::ask_target() {
            return Some(Self::AskForSecret);
        }
        if person_vault::described_use(target).is_some() && *target != person_vault::page_target() {
            return Some(Self::Use);
        }
        if target.provider == person_connections::PROVIDER {
            return Some(Self::Connections);
        }
        if target.provider != person_accounts::PROVIDER {
            return None;
        }
        let (resource, action) = (&target.address.resource, &target.address.action);
        #[cfg(feature = "agents")]
        if resource == CALENDARS {
            return Some(if action == CALENDAR_CONNECT {
                Self::CalendarConnect
            } else {
                Self::Calendars
            });
        }
        if resource != person_accounts::RESOURCE {
            return None;
        }
        #[cfg(feature = "agents")]
        if action == MAIL_CONNECT {
            return Some(Self::MailConnect);
        }
        #[cfg(not(feature = "agents"))]
        let _ = action;
        Some(Self::Mail)
    }
}

/// Whether the device's dispatch answers `target`, called by `caller`, as something an agent's
/// turn asks of what its workspace keeps for the person who asked it
/// ([`Workspaces::answer_turn`]): every agent on a turn asks those there, whatever is then
/// decided for it.
#[must_use]
pub fn answers(caller: &Caller, target: &RemoteActionTarget) -> bool {
    on_turn(caller) && takes(target)
}

/// Whether a call of `target` is one an agent's turn asks the device's dispatch for, of what
/// its workspace keeps for the person who asked it.
#[must_use]
pub fn takes(target: &RemoteActionTarget) -> bool {
    TurnCall::of(target).is_some()
}

/// The providers whose actions an agent's turn is offered of what its workspace keeps for the
/// person who asked it ([`Workspaces::turn_rows`]).
pub const TURN_PROVIDERS: [&str; 4] = [
    person_vault::PROVIDER,
    person_accounts::PROVIDER,
    model_providers::PROVIDER,
    person_connections::PROVIDER,
];

/// Whether `caller` is an agent working a turn.
fn on_turn(caller: &Caller) -> bool {
    caller.acting().is_agent() && caller.turn().is_some()
}

/// The turn an agent works as the workspace's list of its agents keeps it: the workspace and
/// its policy, which the agent's own answers decide with.
struct KeptTurn {
    workspace: WorkspaceId,
    policy: WorkspacePolicy,
}

impl KeptTurn {
    /// The turn as `caller` works it.
    fn of<'a>(&'a self, caller: &'a Caller) -> HostedTurn<'a> {
        HostedTurn {
            workspace: self.workspace,
            caller,
            policy: Some(&self.policy),
        }
    }
}

impl Workspaces {
    /// The turn `caller` works, as its workspace's list of its agents keeps the agent; `None`
    /// for a caller that is no agent in that list.
    async fn kept_turn(&self, caller: &Caller) -> Option<KeptTurn> {
        let workspace = caller.workspace().and_then(WorkspaceId::parse)?;
        let agent = caller.acting().agent_id()?;
        let named = workspace.to_string();
        let policy = self.policy_of(&named).await?;
        if !self.agent_is_active(&named, agent).await {
            return None;
        }
        Some(KeptTurn { workspace, policy })
    }

    /// Answer `request`, what an agent's turn asks of what its workspace keeps for the person
    /// who asked it, for `caller`, the agent on that turn. What narrows the agent is what the
    /// workspace's list of its agents keeps for it now; an agent the list does not name reaches
    /// nothing. A use its person must allow first is refused with what they will be shown
    /// ([`ActionCallError::ConfirmationRequired`]), and their approve admits the same call again.
    ///
    /// # Errors
    /// [`UnknownAction`] for a caller that is no agent on a turn in a workspace's list, or a
    /// call none of these answers; otherwise the refusal of whichever answers it.
    pub(crate) async fn answer_turn(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Result<tilo_actions::remote::InvocationReceipt, ActionCallError> {
        let Some(call) = TurnCall::of(&request.target) else {
            return Err(UnknownAction);
        };
        let Some(kept) = self.kept_turn(caller).await else {
            return Err(UnknownAction);
        };
        let turn = kept.of(caller);
        let value = match call {
            TurnCall::Use => self.use_for_turn(&turn, request).await?,
            TurnCall::AskForSecret => self.secret_for_turn(&turn, request).await?,
            TurnCall::Mail => self.mail_for_turn(&turn, request).await?,
            #[cfg(feature = "agents")]
            TurnCall::MailConnect => answered(self.connect_mail_for_turn(&turn, request).await?)?,
            #[cfg(feature = "agents")]
            TurnCall::Calendars => self.calendars_for_turn(&turn, request).await?,
            #[cfg(feature = "agents")]
            TurnCall::CalendarConnect => {
                answered(self.connect_calendar_for_turn(&turn, request).await?)?
            }
            TurnCall::Connections => self.connections_for_turn(&turn, request).await?,
        };
        Ok(tilo_actions::remote::InvocationReceipt {
            invocation: request.invocation,
            status: tilo_actions::remote::ReceiptStatus::Completed {
                output: tilo_actions::ActionOutput {
                    value: Some(value),
                    resources: Vec::new(),
                },
            },
        })
    }

    #[cfg(feature = "agents")]
    /// The actions of `provider` that `caller`'s turn is offered of what its workspace keeps
    /// for the person who asked it: each one it would not be refused before anything is opened,
    /// read anew at every listing, so a connection its person added a moment ago is in it. A
    /// caller that is no agent in its workspace's list is offered nothing.
    pub(crate) async fn turn_rows(&self, caller: &Caller, provider: &str) -> Vec<CatalogueRow> {
        let Some(kept) = self.kept_turn(caller).await else {
            return Vec::new();
        };
        let turn = kept.of(caller);
        let mut rows = self.uses_offered(&turn).await;
        rows.extend(self.calls_offered(&turn).await);
        if self.secret_ask_for_turn(&turn).await.is_ok() {
            rows.push(person_vault::ask_row());
        }
        rows.extend(self.mail_offered(&turn).await);
        #[cfg(feature = "agents")]
        {
            rows.extend(self.mail_connect_offered(&turn).await);
            rows.extend(self.calendars_offered(&turn).await);
            rows.extend(self.calendar_connect_offered(&turn).await);
        }
        rows.extend(self.connection_rows_for_turn(&turn).await);
        rows.retain(|row| row.provider == provider);
        rows
    }

    #[cfg(feature = "agents")]
    /// Whether `caller`'s turn may ask for a page's field to be filled from its workspace's
    /// vault at all: what [`Self::release_for`] refuses before the vault is opened, asked before
    /// any window is.
    ///
    /// # Errors
    /// As [`Self::may_fill_pages`], and [`UnknownAction`] for a caller that is no agent in its
    /// workspace's list.
    pub(crate) async fn may_fill_pages_for(&self, caller: &Caller) -> Result<(), ActionCallError> {
        let kept = self.kept_turn(caller).await.ok_or(UnknownAction)?;
        self.may_fill_pages(&kept.of(caller)).await
    }

    #[cfg(feature = "agents")]
    /// Release what the item `fill` names keeps for the page at its site, for `caller`'s turn,
    /// which `request` asked, as [`Self::release_for_turn`] decides it for the turn the
    /// workspace's list of its agents keeps.
    ///
    /// # Errors
    /// As [`Self::release_for_turn`], and [`UnknownAction`] for a caller that is no agent in
    /// its workspace's list.
    pub(crate) async fn release_for(
        &self,
        caller: &Caller,
        fill: &FillPage,
        request: &InvokeRequest,
    ) -> Result<PageText, ActionCallError> {
        let kept = self.kept_turn(caller).await.ok_or(UnknownAction)?;
        self.release_for_turn(&kept.of(caller), fill, request).await
    }
}

/// `value`, what a turn's call came to, as the dispatch answers it.
#[cfg(feature = "agents")]
fn answered(value: impl serde::Serialize) -> Result<Value, ActionCallError> {
    serde_json::to_value(value).map_err(|error| ActionCallError::refused(error.to_string()))
}

/// What uses of a vault item by reference keep on this server: what proves a
/// sign-in at its server before an account is kept.
pub(super) struct Uses {
    pub(super) prover: std::sync::RwLock<std::sync::Arc<dyn tilo_account_probe::AccountTester>>,
}

/// The kind of field a released text is for, in the words a window's engine
/// knows it by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageField {
    /// A login's password: only a password field takes it.
    Password,
    /// A code generator's code of this moment.
    Code,
    /// A kept value.
    Value,
}

impl PageField {
    /// The engine's word for it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Password => "password",
            Self::Code => "code",
            Self::Value => "value",
        }
    }
}

/// A text this server released for one site, for whoever sends it on to the
/// window that holds that site's page. It prints nothing of the text, and
/// what holds it is wiped when it is dropped.
#[derive(Debug)]
pub enum PageText {
    /// What a login or a value keeps.
    Item(vault_items::Released),
    /// A code generator's code of this moment.
    Code(vault_items::Code),
}

impl PageText {
    /// The text, for the one write into the page.
    #[must_use]
    pub fn text(&self) -> &str {
        match self {
            Self::Item(released) => released.secret.as_str(),
            Self::Code(code) => code.digits(),
        }
    }

    /// The kind of field it is for.
    #[must_use]
    pub fn field(&self) -> PageField {
        match self {
            Self::Item(released) => match released.kind {
                vault_items::ItemKind::Value => PageField::Value,
                _ => PageField::Password,
            },
            Self::Code(_) => PageField::Code,
        }
    }
}

impl Default for Uses {
    fn default() -> Self {
        Self {
            prover: std::sync::RwLock::new(std::sync::Arc::new(
                tilo_account_probe::FindingAccountTester(tilo_account_probe::imap::Reach::any()),
            )),
        }
    }
}

impl Uses {
    /// The uses of a server: an account's sign-in is proved over connections
    /// that go only where `reach` lets them.
    pub(super) fn proving_within(reach: tilo_account_probe::imap::Reach) -> Self {
        Self {
            prover: std::sync::RwLock::new(std::sync::Arc::new(
                tilo_account_probe::FindingAccountTester(reach),
            )),
        }
    }
}

/// Who asked a turn on this server, and which agent carries it out: what
/// [`Workspaces::asked_turn`] answers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AskedTurn {
    /// The person who asked the turn, whom it acts for: their key.
    pub person: [u8; 32],
    /// The workspace, as the caller names it.
    pub workspace: String,
    /// The agent, by its id.
    pub agent: String,
    /// The agent's name, as its person reads it.
    pub name: String,
}

impl AskedTurn {
    /// The agent acting for its person: who asked for the writes made on this turn
    /// ([`Workspaces::acting_or_own`] says whom each names).
    #[must_use]
    pub fn acting(&self) -> tilo_action_policy::Acting {
        tilo_action_policy::Acting::agent(self.agent.as_str())
            .acting_for(tilo_action_policy::Acting::person(hex::encode(self.person)))
    }
}

/// The code of this moment a code generator's seed makes, read where the vault is: a use that
/// changes nothing, so it is written down nowhere and its item's history stays as it was.
#[derive(Clone, Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ReadCode {
    /// Why whoever asks says it needs the code.
    #[serde(default)]
    reason: String,
}

/// One use by reference this server carries out.
enum Use {
    /// A mail account added with the sign-in an item keeps.
    Mail(AddMailWithItem),
    /// A provider's key kept from an item.
    Key(SetKeyFromItem),
    /// A web call made with a key.
    Call(web_call::WebCall),
    /// The code of this moment from a code generator's seed.
    Code(ReadCode),
}

impl Use {
    /// The use `target` names with `arguments`. The reason never quotes the
    /// arguments.
    fn read(
        target: &RemoteActionTarget,
        arguments: &Map<String, Value>,
    ) -> Result<Self, ActionCallError> {
        let invalid = |what: &str| InvalidRequest {
            reason: what.to_owned(),
        };
        let arguments = Value::Object(arguments.clone());
        match (target.provider.as_str(), target.address.action.as_str()) {
            (person_accounts::PROVIDER, person_accounts::ADD_MAIL_WITH_ITEM) => {
                serde_json::from_value(arguments).map(Self::Mail).map_err(|_| {
                    invalid(
                        "a mail account to add names its address, its servers and the vault item that is its password",
                    )
                })
            }
            (model_providers::PROVIDER, model_providers::SET_KEY_FROM_ITEM) => {
                serde_json::from_value(arguments).map(Self::Key).map_err(|_| {
                    invalid("a key to keep names its provider and the vault item that is the key")
                })
            }
            (person_vault::PROVIDER, person_vault::WEB_CALL) => {
                serde_json::from_value(arguments).map(Self::Call).map_err(|_| {
                    invalid("a web call names its address and the vault item that is its key")
                })
            }
            (person_vault::PROVIDER, person_vault::CODE) => serde_json::from_value(arguments)
                .map(Self::Code)
                .map_err(|_| invalid("reading a code takes the reason it is needed, if any")),
            _ => Err(UnknownAction),
        }
    }

    /// What it wants of the vault under `root`: the item, the kind it must
    /// be, and every place its secret would be sent to.
    fn wanted(
        &self,
        root: &Path,
        target: &tilo_actions::remote::RemoteActionTarget,
    ) -> Result<(ItemId, vault_items::ItemKind, Vec<Place>), String> {
        let id = |item: &ItemRef| {
            ItemId::parse(item.id()).ok_or_else(|| vault_items::ItemError::NotFound.to_string())
        };
        match self {
            Self::Mail(adding) => {
                let server = |scheme: &str, name: &str, port: u16| {
                    Place::server(scheme, name, port).map_err(|error| error.to_string())
                };
                Ok((
                    id(&adding.password)?,
                    vault_items::ItemKind::Login,
                    vec![
                        server("imap", &adding.incoming_host, adding.incoming_port)?,
                        server("smtp", &adding.outgoing_host, adding.outgoing_port)?,
                    ],
                ))
            }
            Self::Key(setting) => {
                let config = super::person_settings::provider_config(root);
                let profile = config
                    .profiles
                    .iter()
                    .find(|profile| profile.id == setting.provider)
                    .ok_or_else(|| "That provider is not kept here".to_owned())?;
                let endpoint =
                    Place::parse(&profile.endpoint).map_err(|error| error.to_string())?;
                Ok((
                    id(&setting.key)?,
                    vault_items::ItemKind::Key,
                    vec![endpoint],
                ))
            }
            Self::Call(call) => web_call::wanted(call),
            // The item the address names is the code generator; it sends nothing anywhere.
            Self::Code(_) => {
                let item = ItemId::parse(
                    target_resource(target, person_vault::ITEM_KIND).unwrap_or_default(),
                )
                .ok_or_else(|| vault_items::ItemError::NotFound.to_string())?;
                Ok((item, vault_items::ItemKind::CodeGenerator, Vec::new()))
            }
        }
    }

    /// Why whoever asks says it needs the item.
    fn reason(&self) -> &str {
        match self {
            Self::Mail(adding) => adding.reason.trim(),
            Self::Key(setting) => setting.reason.trim(),
            Self::Call(call) => call.reason.trim(),
            Self::Code(code) => code.reason.trim(),
        }
    }

    /// Whether its person's "Always" may stand for it: not for a call,
    /// whose request the agent writes anew each time.
    const fn takes_leave(&self) -> bool {
        !matches!(self, Self::Call(_))
    }

    /// Whether `items` holds what it wants: for a code, a code generator, which needs no place
    /// to make a code at; every other use is held to the places it sends its secret to.
    fn fits(
        &self,
        items: &VaultItems,
        item: &ItemId,
        kind: vault_items::ItemKind,
        places: &[Place],
    ) -> Result<(), vault_items::ItemError> {
        match self {
            Self::Code(_) => {
                let _ = (kind, places);
                items.code_params(item).map(|_| ())
            }
            Self::Mail(_) | Self::Key(_) | Self::Call(_) => {
                items.fits(item, kind, places).map(|_| ())
            }
        }
    }

    /// What else it would be refused for when it is carried out, with the
    /// vault's `items`: said before anyone is asked about it.
    fn sendable(&self, workspaces: &Workspaces, items: &VaultItems) -> Result<(), String> {
        match self {
            Self::Call(call) => web_call::sendable(call, workspaces.call_reach(), items),
            Self::Mail(_) | Self::Key(_) | Self::Code(_) => Ok(()),
        }
    }

    /// Carry it out for `by` with what `workspace`'s vault keeps, its other stores under
    /// `root`: the one place the reference becomes its secret, inside the code that signs in.
    /// What it is used for and how its person allowed it (`purpose`) is written down with the
    /// use.
    async fn carry_out(
        self,
        workspaces: &Workspaces,
        (workspace, root): (&str, &Path),
        (by, acting, item, kind, places): (
            &str,
            &tilo_action_policy::Acting,
            ItemId,
            vault_items::ItemKind,
            Vec<Place>,
        ),
        purpose: UsedFor<'_>,
    ) -> Result<Value, String> {
        match self {
            Self::Mail(adding) => {
                let kept = workspaces
                    .mail_with_item((workspace, root), adding, (by, acting), purpose)
                    .await?;
                workspaces.person_settings_changed();
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
            Self::Key(setting) => {
                let kept = workspaces
                    .key_from_item((workspace, root), &setting, (by, acting), purpose)
                    .await?;
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
            Self::Call(call) => web_call::send(workspaces, workspace, call, (by, acting), purpose).await,
            Self::Code(_) => {
                // Read where the vault is, as the turn's person: nothing is written, so no use
                // stands in the item's history.
                let _ = purpose;
                let reference = format!("{}:{}", person_vault::ITEM_KIND, item.as_str());
                let code = workspaces.vault_code(workspace, &reference).await?;
                let now = tilo_clock::unix_now();
                Ok(json!({
                    "code": code.digits(),
                    "left": code.until_unix.saturating_sub(now),
                    "period": code.params.period,
                    "digits": code.params.digits,
                    "algorithm": code.params.algorithm.word(),
                }))
            }
        }
    }
}

#[cfg(feature = "agents")]
/// The item and the site a page fill names. A text is written only into a
/// page that arrived over https.
fn page_wanted(fill: &FillPage) -> Result<(ItemId, Place), String> {
    let item = ItemId::parse(fill.item.id())
        .ok_or_else(|| vault_items::ItemError::NotFound.to_string())?;
    let site = Place::parse(&fill.site).map_err(|error| error.to_string())?;
    if site.scheme() != "https" {
        return Err("A vault item is filled only into a page that arrived over https".to_owned());
    }
    Ok((item, site))
}

#[cfg(feature = "agents")]
/// The kind of field an item of `kind` fills, when it fills one.
fn page_field(kind: vault_items::ItemKind) -> Result<PageField, String> {
    match kind {
        vault_items::ItemKind::Login => Ok(PageField::Password),
        vault_items::ItemKind::CodeGenerator => Ok(PageField::Code),
        vault_items::ItemKind::Value => Ok(PageField::Value),
        vault_items::ItemKind::Key | vault_items::ItemKind::SshKey => {
            Err("Only a login, a code generator or a value is filled into a page".to_owned())
        }
    }
}

#[cfg(feature = "agents")]
/// Hand out what the item `item` of `workspace`'s vault fills a page's field at `site` with, for
/// `by`: a code generator's code of this moment, else what the item keeps. The use is written
/// down first, as every use is.
async fn page_text(
    workspaces: &Workspaces,
    workspace: &str,
    (item, kind): (&ItemId, vault_items::ItemKind),
    site: &Place,
    (by, asker): (&str, &tilo_action_policy::Acting),
) -> Result<PageText, String> {
    workspaces
        .changing_vault(workspace, Some(asker), |items| match kind {
            vault_items::ItemKind::CodeGenerator => {
                items.code_at(item, site, by).map(PageText::Code)
            }
            kind => items
                .use_as(item, kind, std::slice::from_ref(site), by)
                .map(PageText::Item),
        })
        .await
}

/// What a person is shown of the site a page fill names: its name, and its
/// origin whole.
fn shown_site(target: &RemoteActionTarget, arguments: &Map<String, Value>) -> Option<Value> {
    if *target != person_vault::page_target() {
        return None;
    }
    let origin = arguments.get("site")?.as_str()?;
    Some(json!({"name": site_name(origin), "origin": origin}))
}

/// The id the address's resource names, after `kind`: `vault.item:<id>` names `<id>`.
fn target_resource<'a>(target: &'a tilo_actions::remote::RemoteActionTarget, kind: &str) -> Option<&'a str> {
    target
        .address
        .resource
        .strip_prefix(kind)
        .and_then(|rest| rest.strip_prefix(':'))
}

/// The name in the origin `origin`: without its scheme and its port.
fn site_name(origin: &str) -> &str {
    let named = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    match named.rsplit_once(':') {
        Some((name, port))
            if !port.is_empty() && port.bytes().all(|digit| digit.is_ascii_digit()) =>
        {
            name
        }
        _ => named,
    }
}

/// The arguments `request` carries.
fn arguments_of(request: &InvokeRequest) -> Map<String, Value> {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => Map::new(),
    }
}

/// Where a grant that lets `using`'s agent make the use `target` names of `item`, sending
/// it to `places`, stands in its workspace's policy.
fn vault_use_node(
    using: &AskedTurn,
    target: &RemoteActionTarget,
    item: &ItemId,
    places: &[Place],
) -> TreePath {
    let mut sorted: Vec<&str> = places.iter().map(Place::as_str).collect();
    sorted.sort_unstable();
    sorted.dedup();
    let resource = if sorted.is_empty() {
        item.as_str().to_owned()
    } else {
        format!("{}/{}", item.as_str(), json!(sorted))
    };
    TreePath::workspace(&using.workspace)
        .app(&target.provider)
        .kind(person_vault::use_word(target))
        .resource(resource)
        .action(&target.address.action)
}

/// What `using`'s person is shown of the use `target` names with `arguments`: why their
/// agent says it needs the item, the item by its kind and the places it would go, and the
/// site of a page. "Always" is offered where a grant may stand for it.
fn use_asked(
    caller: &Caller,
    target: &RemoteActionTarget,
    arguments: &Map<String, Value>,
    (kind, places, reason): (vault_items::ItemKind, &[Place], &str),
    takes_leave: bool,
) -> AskedFacts {
    let mut facts = vec![
        Fact::new(
            "Item",
            serde_json::to_value(kind)
                .ok()
                .and_then(|kind| kind.as_str().map(str::to_owned))
                .unwrap_or_default(),
        ),
        Fact::new(
            "Goes to",
            places
                .iter()
                .map(Place::as_str)
                .collect::<Vec<_>>()
                .join(" "),
        ),
    ];
    if let Some(site) = shown_site(target, arguments) {
        facts.push(Fact::new(
            "Site",
            site.get("origin")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ));
    }
    let said = |name: &str| {
        arguments
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
    };
    // What the call sends where, as the person would read it in its log: the method and the
    // address, never the key.
    if *target == person_vault::call_target() {
        let method = match said("method").trim() {
            "" => "GET".to_owned(),
            method => method.to_ascii_uppercase(),
        };
        facts.push(Fact::new("Calls", format!("{method} {}", said("url"))));
    }
    // The account it signs in, where it signs one in.
    if *target == person_accounts::target(person_accounts::ADD_MAIL_WITH_ITEM) {
        facts.push(Fact::new("Account", said("address")));
    }
    AskedFacts {
        words: reason.to_owned(),
        facts,
        verb: person_vault::use_word(target),
        plural: takes_leave.then(|| "vault items".to_owned()),
        ..AskedFacts::by(super::asking::asked_by(caller))
    }
}

impl Workspaces {
    #[cfg(feature = "agents")]
    /// Who asked the turn `caller` works in `workspace`, where it is one this server carries
    /// out ([`Self::asked_turn`]): what a provider of the turn's own checks first.
    ///
    /// # Errors
    /// [`UnknownAction`] for every other caller.
    pub(crate) async fn asked_turn_of(
        &self,
        workspace: WorkspaceId,
        caller: &Caller,
    ) -> Result<AskedTurn, ActionCallError> {
        self.asked_turn(&HostedTurn {
            workspace,
            caller,
            policy: None,
        })
        .await
    }

    /// Who asked `turn`, where it is one this server carries out: the first
    /// check of everything of the workspace's that such a turn reaches.
    /// Unknown and forbidden are the same answer.
    ///
    /// It holds where the caller is one of this server's agents as its own
    /// approved key, working on a turn for the person who asked, and no app;
    /// the caller's workspace is the turn's; and the workspace admits that
    /// person now. What the turn may then reach is the workspace policy's
    /// word on the agent ([`Self::turn_reaches`]), decided after this one.
    ///
    /// # Errors
    /// [`UnknownAction`] for every other caller.
    pub(crate) async fn asked_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<AskedTurn, ActionCallError> {
        let caller = turn.caller;
        let acting = caller.acting();
        // This server's own agent, inside a turn: nothing else is a turn
        // this server carries out.
        if acting.app().is_some()
            || !acting.is_agent()
            || caller.roster_created_at().is_none()
            || caller.turn().is_none()
        {
            return Err(UnknownAction);
        }
        let (Some(person), Some(agent), Some(workspace)) = (
            caller.turn_person().copied(),
            acting.agent_id(),
            caller.workspace(),
        ) else {
            return Err(UnknownAction);
        };
        if WorkspaceId::parse(workspace) != Some(turn.workspace) {
            return Err(UnknownAction);
        }
        // A person the workspace no longer admits leaves the turns they asked nothing.
        if !self.admits_person(turn.workspace, person).await {
            return Err(UnknownAction);
        }
        Ok(AskedTurn {
            person,
            workspace: workspace.to_owned(),
            agent: agent.to_owned(),
            name: caller.agent_name().unwrap_or(agent).to_owned(),
        })
    }

    /// Whether `turn` may ask its person for a secret ([`person_vault::ASK`]): the turn is one
    /// this server carries out, its agent reaches the vault, and an agent that anything
    /// narrows holds a rule that names the ask. The turn as the entry knows it.
    ///
    /// # Errors
    /// As [`Self::asked_turn`], and [`NotPermitted`] for a narrowed agent without that rule.
    pub(crate) async fn secret_ask_for_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<AskedTurn, ActionCallError> {
        self.turn_uses_for(turn, &person_vault::ask_target()).await
    }

    /// Ask the person `turn` acts for for the secret `request` names: it waits for them to type
    /// it into their vault through the vault's own add, in the system's protected field and in
    /// the system's words, and once they did it answers the reference it is kept under. Nothing
    /// here sees the secret.
    ///
    /// # Errors
    /// As [`Self::secret_ask_for_turn`]; the arguments do not read; or it waits for the person
    /// ([`ActionCallError::ConfirmationRequired`]).
    async fn secret_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let using = self.secret_ask_for_turn(turn).await?;
        let InvokeArgument::Json(arguments) = &request.argument else {
            return Err(InvalidRequest {
                reason: "an ask for a secret names its kind and where it belongs".to_owned(),
            });
        };
        let asked: person_vault::AskForItem =
            serde_json::from_value(Value::Object(arguments.clone())).map_err(|_| {
                InvalidRequest {
            reason: "an ask for a secret names its kind (login, key, code-generator, ssh-key or \
                     value), belongsTo and, when known, username, and never the secret"
                .to_owned(),
        }
            })?;
        let asked = asked
            .checked()
            .map_err(|reason| InvalidRequest { reason })?;
        if request.approval().is_none() {
            return Err(ActionCallError::asks(super::person_vault::secret_asked(
                turn.caller,
                &using.name,
                &asked,
            )));
        }
        let reference = self
            .asked_item(&using.workspace, &asked)
            .await
            .map_err(ActionCallError::refused)?;
        serde_json::to_value(person_vault::Asked { reference })
            .map_err(|error| ActionCallError::refused(error.to_string()))
    }

    /// [`Self::asked_turn`] for a use by reference: an agent that anything
    /// narrows also holds a rule that names `target`.
    async fn turn_uses_for(
        &self,
        turn: &HostedTurn<'_>,
        target: &RemoteActionTarget,
    ) -> Result<AskedTurn, ActionCallError> {
        let using = self.asked_turn(turn).await?;
        if !turn.may_take(&using.workspace, target) {
            return Err(NotPermitted);
        }
        // The workspace's vault is used only by whom its policy lets use it.
        if !self
            .turn_reaches(
                turn,
                Store::VAULT,
                &target.address.action,
                ActionKind::Undoable,
            )
            .await
        {
            return Err(NotPermitted);
        }
        Ok(using)
    }

    #[cfg(feature = "agents")]
    /// The uses by reference `turn` is offered, as a catalogue lists them:
    /// each one it would not be refused before the vault is opened.
    pub(crate) async fn uses_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let mut offered = Vec::new();
        for row in person_vault::use_rows() {
            let Some(action) = &row.action else {
                continue;
            };
            let target = RemoteActionTarget {
                provider: row.provider.clone(),
                address: action.address.clone(),
            };
            if self.turn_uses_for(turn, &target).await.is_ok() {
                offered.push(row);
            }
        }
        offered
    }

    #[cfg(feature = "agents")]
    /// The calls that send what a vault item keeps out with a request of
    /// the agent's own making, which `turn` is offered as a catalogue lists
    /// them: each one it would not be refused before the vault is opened.
    /// They are listed apart from [`Self::uses_offered`]: no "Always"
    /// stands for one.
    pub(crate) async fn calls_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let mut offered = Vec::new();
        for row in person_vault::call_rows() {
            let Some(action) = &row.action else {
                continue;
            };
            let target = RemoteActionTarget {
                provider: row.provider.clone(),
                address: action.address.clone(),
            };
            if self.turn_uses_for(turn, &target).await.is_ok() {
                offered.push(row);
            }
        }
        offered
    }

    /// Take the use by reference `request` asks for, for `turn`: carried out where a grant
    /// of the workspace's lets this agent make this use of the item at these places, or where
    /// its person approved this very call; otherwise it waits for their yes.
    ///
    /// # Errors
    /// The turn is none this server carries out ([`UnknownAction`]), its agent is narrowed and
    /// holds no rule for this use ([`NotPermitted`]), the arguments do not read, the vault
    /// refuses the use (no such item in the workspace's vault, another kind, a place it does
    /// not belong to), or it waits for the person's yes
    /// ([`ActionCallError::ConfirmationRequired`]).
    pub(crate) async fn use_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let using = self.turn_uses_for(turn, &request.target).await?;
        let arguments = arguments_of(request);
        let wanted = Use::read(&request.target, &arguments)?;
        let root = self.workspace_root(&using.workspace);
        let (item, kind, places) = wanted.wanted(&root, &request.target).map_err(ActionCallError::refused)?;
        let word = person_vault::use_word(&request.target);
        let items = self
            .open_vault(&using.workspace)
            .await
            .map_err(ActionCallError::refused)?
            .items();
        wanted
            .fits(&items, &item, kind, &places)
            .map_err(|error| ActionCallError::refused(error.to_string()))?;
        wanted
            .sendable(self, &items)
            .map_err(ActionCallError::refused)?;
        drop(items);
        let granted = wanted.takes_leave()
            && self
                .granted(
                    &using.workspace,
                    &using.agent,
                    &vault_use_node(&using, &request.target, &item, &places),
                )
                .await;
        let allowed = if granted {
            Allowance::Always
        } else {
            super::asking::approved_or_asks(request, || {
                use_asked(
                    turn.caller,
                    &request.target,
                    &arguments,
                    (kind, &places, wanted.reason()),
                    wanted.takes_leave(),
                )
            })?;
            Allowance::Once
        };
        let purpose = UsedFor {
            using: &word,
            what: "",
            allowed: Some(allowed),
        };
        wanted
            .carry_out(
                self,
                (&using.workspace, &root),
                (&using.name, &using.acting(), item, kind, places),
                purpose,
            )
            .await
            .map_err(ActionCallError::refused)
    }
}

/// The device of `person`'s own the workspace `peer` holds certified most recently and admits
/// now: the one an agent this server runs for them works on, so what the agent may do is the
/// person's access here at every step, and a person removed from the workspace leaves their
/// agent nothing.
///
/// A device this server placed itself (`placed`) is never that device. It is certified in the
/// person's name for a window in the workspace's machine, later than any device of their own,
/// and it is confined to that window: a person with no other device here has none.
pub(super) fn person_device_in(
    peer: &super::StatePeer,
    person: &[u8; 32],
    placed: impl Fn(&[u8; 32]) -> bool,
) -> Option<[u8; 32]> {
    let now = tilo_clock::wall();
    peer.certificates()
        .iter()
        .filter(|cert| &cert.user_public_key == person)
        .filter(|cert| {
            peer.governance()
                .governed_agent(&cert.device_public_key)
                .is_none()
        })
        .filter(|cert| !placed(&cert.device_public_key))
        .filter(|cert| peer.permissions(cert.device_public_key, now).is_ok())
        .max_by_key(|cert| cert.issued_at_ns)
        .map(|cert| cert.device_public_key)
}

impl Workspaces {
    /// [`person_device_in`] the workspace `workspace` governed here: `None` for
    /// a workspace not governed here, or a person it admits on no device of their own. The
    /// devices this server placed are those its own list of them holds
    /// ([`crate::environment::EnvironmentRegistry`]).
    pub async fn person_device(
        &self,
        workspace: WorkspaceId,
        person: [u8; 32],
    ) -> Option<[u8; 32]> {
        self.device_beside(workspace, person, None).await
    }

    /// The device of `person`'s own that something read for them alone acts as in `workspace`:
    /// the one it certified most recently and admits now that is neither this server's own
    /// device nor one this server placed.
    ///
    /// This server's own device is a member of every workspace it governs, and on a person's
    /// own server it is certified in that person's name: [`Self::person_device`] answers it
    /// where it is their newest, and a turn in that workspace acts as it. A read for a person
    /// in another workspace of theirs never does, or it would read with the server's own
    /// membership there. `None` for a workspace not governed here, or a person it admits on
    /// no other device.
    pub async fn persons_own_device(
        &self,
        workspace: WorkspaceId,
        person: [u8; 32],
    ) -> Option<[u8; 32]> {
        let never = self.device_cert().device_public_key;
        self.device_beside(workspace, person, Some(never)).await
    }

    /// [`person_device_in`] `workspace`, where neither a device this server
    /// placed nor `never` is the one.
    async fn device_beside(
        &self,
        workspace: WorkspaceId,
        person: [u8; 32],
        never: Option<[u8; 32]>,
    ) -> Option<[u8; 32]> {
        let placed = &self.inner.host.environments;
        let now_ns = tilo_clock::unix_now_ns();
        self.governing(workspace, |governance| {
            person_device_in(governance.peer, &person, |device| {
                never.as_ref() == Some(device) || placed.of(device, now_ns).is_some()
            })
        })
        .await
        .flatten()
    }

    /// Whether `workspace`, governed here, admits `person` on a device now.
    async fn admits_person(&self, workspace: WorkspaceId, person: [u8; 32]) -> bool {
        let placed = &self.inner.host.environments;
        let now_ns = tilo_clock::unix_now_ns();
        self.governing(workspace, |governance| {
            person_device_in(governance.peer, &person, |device| {
                placed.of(device, now_ns).is_some()
            })
            .is_some()
        })
        .await
        .unwrap_or(false)
    }
}

impl Workspaces {
    #[cfg(feature = "agents")]
    /// Whether `turn` may ask for a page's field to be filled from its workspace's vault at
    /// all: what [`Self::release_for_turn`] refuses before the vault is opened, asked before
    /// any window is.
    ///
    /// # Errors
    /// As [`Self::asked_turn`], and [`NotPermitted`] for a narrowed agent that holds no
    /// rule for this use.
    pub(crate) async fn may_fill_pages(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<(), ActionCallError> {
        self.turn_uses_for(turn, &person_vault::page_target())
            .await
            .map(drop)
    }

    #[cfg(feature = "agents")]
    /// Release what the item `fill` names keeps, for the page at the site `fill` names, to
    /// whoever sends it on to that page's window, for `turn`, which `request` asked: at once
    /// where a grant of the workspace's lets this agent fill it there, or where its person
    /// approved this very call; otherwise it waits for their yes. The use is written down
    /// before anything is handed out.
    ///
    /// # Errors
    /// As [`Self::use_for_turn`], and the vault's own refusals: no such item in the
    /// workspace's vault, a site it does not belong to or that did not arrive over https, an
    /// item that is no login, code generator or value.
    pub(crate) async fn release_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        fill: &FillPage,
        request: &InvokeRequest,
    ) -> Result<PageText, ActionCallError> {
        let target = person_vault::page_target();
        let using = self.turn_uses_for(turn, &target).await?;
        let (item, site) = page_wanted(fill).map_err(ActionCallError::refused)?;
        let kind = {
            let items = self
                .open_vault(&using.workspace)
                .await
                .map_err(ActionCallError::refused)?
                .items();
            let facts = items
                .belongs_at(&item, &site)
                .map_err(|error| ActionCallError::refused(error.to_string()))?;
            page_field(facts.kind).map_err(ActionCallError::refused)?;
            facts.kind
        };
        let places = std::slice::from_ref(&site);
        if !self
            .granted(
                &using.workspace,
                &using.agent,
                &vault_use_node(&using, &target, &item, places),
            )
            .await
        {
            let mut arguments = Map::new();
            arguments.insert("site".into(), json!(site.as_str()));
            super::asking::approved_or_asks(request, || {
                use_asked(
                    turn.caller,
                    &target,
                    &arguments,
                    (kind, places, fill.reason.trim()),
                    true,
                )
            })?;
        }
        page_text(
            self,
            &using.workspace,
            (&item, kind),
            &site,
            (&using.name, &using.acting()),
        )
        .await
        .map_err(ActionCallError::refused)
    }
}
