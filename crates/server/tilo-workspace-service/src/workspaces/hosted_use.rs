//! A secret used by reference by a turn this server carries out itself.
//!
//! An agent this server runs works with every device of whoever asked it off, so no transport
//! verified a device behind what it asks, and the check a device's request passes
//! ([`Workspaces::reaching`]) has nothing to compare. This is the server's own entry for such a
//! turn. It is reached from no transport: only the code that carries out the turn calls it, with
//! the very caller the turn's tools act as and what the workspace's list of its agents says of the
//! agent ([`HostedTurn`]).
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
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_action_policy::{ActionKind, Answer, Rule, TreePath};
use tilo_actions::remote::ActionCallError::{self, InvalidRequest, NotPermitted, UnknownAction};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_accounts::{self, AddMailWithItem};
use tilo_actions::remote::person_models::{self, SetKeyFromItem};
use tilo_actions::remote::person_vault::{self, FillPage, ItemRef};
use tilo_actions::remote::{
    Caller, CatalogueRow, InvokeArgument, InvokeRequest, RemoteActionTarget,
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
pub struct HostedTurn<'a> {
    /// The workspace the turn works in.
    pub workspace: WorkspaceId,
    /// The caller the turn's tools act as: the asking person's session,
    /// handed to the agent on this turn.
    pub caller: &'a Caller,
    /// The rules the turn is decided by, where anything narrows its agent:
    /// what its entry bounds it to and, for a sub-agent, what keeps it from
    /// whoever started it. `None` for an agent nothing narrows.
    pub bound: Option<&'a [Rule]>,
}

impl HostedTurn<'_> {
    /// Whether the rules that narrow the turn's agent let it take `target`
    /// in `workspace`: an agent nothing narrows may, a narrowed one only
    /// where a rule names this very action and does not deny it.
    #[must_use]
    pub fn may_take(&self, workspace: &str, target: &RemoteActionTarget) -> bool {
        bound_allows(self.bound, workspace, target)
    }
}

/// Whether the rules `bound` narrow a turn to let it take `target` in `workspace`: an agent
/// nothing narrows (`None`) may, a narrowed one only where a rule names this very action and
/// does not deny it.
#[must_use]
pub fn bound_allows(bound: Option<&[Rule]>, workspace: &str, target: &RemoteActionTarget) -> bool {
    let Some(bound) = bound else {
        return true;
    };
    let node = TreePath::workspace(workspace)
        .app(target.provider.clone())
        .kind(target.address.resource.clone())
        .resource(target.address.resource.clone())
        .action(target.address.action.clone());
    let granted = tilo_automation::grant_answer(bound, &node, ActionKind::Outside);
    granted.names_action && granted.answer != Answer::Deny
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

/// One use by reference this server carries out.
enum Use {
    /// A mail account added with the sign-in an item keeps.
    Mail(AddMailWithItem),
    /// A provider's key kept from an item.
    Key(SetKeyFromItem),
    /// A web call made with a key.
    Call(web_call::WebCall),
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
            (person_models::PROVIDER, person_models::SET_KEY_FROM_ITEM) => {
                serde_json::from_value(arguments).map(Self::Key).map_err(|_| {
                    invalid("a key to keep names its provider and the vault item that is the key")
                })
            }
            (person_vault::PROVIDER, person_vault::WEB_CALL) => {
                serde_json::from_value(arguments).map(Self::Call).map_err(|_| {
                    invalid("a web call names its address and the vault item that is its key")
                })
            }
            _ => Err(UnknownAction),
        }
    }

    /// What it wants of the vault under `root`: the item, the kind it must
    /// be, and every place its secret would be sent to.
    fn wanted(&self, root: &Path) -> Result<(ItemId, vault_items::ItemKind, Vec<Place>), String> {
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
        }
    }

    /// Why whoever asks says it needs the item.
    fn reason(&self) -> &str {
        match self {
            Self::Mail(adding) => adding.reason.trim(),
            Self::Key(setting) => setting.reason.trim(),
            Self::Call(call) => call.reason.trim(),
        }
    }

    /// Whether its person's "Always" may stand for it: not for a call,
    /// whose request the agent writes anew each time.
    const fn takes_leave(&self) -> bool {
        !matches!(self, Self::Call(_))
    }

    /// What else it would be refused for when it is carried out, with the
    /// vault's `items`: said before anyone is asked about it.
    fn sendable(&self, workspaces: &Workspaces, items: &VaultItems) -> Result<(), String> {
        match self {
            Self::Call(call) => web_call::sendable(call, workspaces.call_reach(), items),
            Self::Mail(_) | Self::Key(_) => Ok(()),
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
        by: &str,
        purpose: UsedFor<'_>,
    ) -> Result<Value, String> {
        match self {
            Self::Mail(adding) => {
                let kept = workspaces
                    .mail_with_item((workspace, root), adding, by, purpose)
                    .await?;
                workspaces.person_settings_changed();
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
            Self::Key(setting) => {
                let kept = workspaces
                    .key_from_item((workspace, root), &setting, by, purpose)
                    .await?;
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
            Self::Call(call) => web_call::send(workspaces, workspace, call, by, purpose).await,
        }
    }
}

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

/// Hand out what the item `item` of `workspace`'s vault fills a page's field at `site` with, for
/// `by`: a code generator's code of this moment, else what the item keeps. The use is written
/// down first, as every use is.
async fn page_text(
    workspaces: &Workspaces,
    workspace: &str,
    (item, kind): (&ItemId, vault_items::ItemKind),
    site: &Place,
    by: &str,
) -> Result<PageText, String> {
    workspaces
        .changing_vault(workspace, |items| match kind {
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
    TreePath::workspace(&using.workspace)
        .app(&target.provider)
        .kind(person_vault::use_word(target))
        .resource(format!("{}/{}", item.as_str(), json!(sorted)))
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
    AskedFacts {
        words: reason.to_owned(),
        facts,
        verb: person_vault::use_word(target),
        plural: takes_leave.then(|| "vault items".to_owned()),
        ..AskedFacts::by(super::asking::asked_by(caller))
    }
}

impl Workspaces {
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
    pub async fn asked_turn(&self, turn: &HostedTurn<'_>) -> Result<AskedTurn, ActionCallError> {
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
    pub async fn secret_ask_for_turn(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Result<AskedTurn, ActionCallError> {
        self.turn_uses_for(turn, &person_vault::ask_target()).await
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

    /// The uses by reference `turn` is offered, as a catalogue lists them:
    /// each one it would not be refused before the vault is opened.
    pub async fn uses_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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

    /// The calls that send what a vault item keeps out with a request of
    /// the agent's own making, which `turn` is offered as a catalogue lists
    /// them: each one it would not be refused before the vault is opened.
    /// They are listed apart from [`Self::uses_offered`]: no "Always"
    /// stands for one.
    pub async fn calls_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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
    pub async fn use_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Value, ActionCallError> {
        let using = self.turn_uses_for(turn, &request.target).await?;
        let arguments = arguments_of(request);
        let wanted = Use::read(&request.target, &arguments)?;
        let root = self.workspace_root(&using.workspace);
        let (item, kind, places) = wanted.wanted(&root).map_err(ActionCallError::refused)?;
        let word = person_vault::use_word(&request.target);
        let items = self
            .open_vault(&using.workspace)
            .await
            .map_err(ActionCallError::refused)?
            .items();
        items
            .fits(&item, kind, &places)
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
            .carry_out(self, (&using.workspace, &root), &using.name, purpose)
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
        let never = self.governor().device_public_key;
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
    /// Whether `turn` may ask for a page's field to be filled from its workspace's vault at
    /// all: what [`Self::release_for_turn`] refuses before the vault is opened, asked before
    /// any window is.
    ///
    /// # Errors
    /// As [`Self::asked_turn`], and [`NotPermitted`] for a narrowed agent that holds no
    /// rule for this use.
    pub async fn may_fill_pages(&self, turn: &HostedTurn<'_>) -> Result<(), ActionCallError> {
        self.turn_uses_for(turn, &person_vault::page_target())
            .await
            .map(drop)
    }

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
    pub async fn release_for_turn(
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
        page_text(self, &using.workspace, (&item, kind), &site, &using.name)
            .await
            .map_err(ActionCallError::refused)
    }
}
