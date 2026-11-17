//! A secret used by reference by a turn this host runs itself.
//!
//! An agent a person keeps on this host works with every device of theirs
//! off, so no transport verified a device behind what it asks, and the gate
//! a device's request passes ([`Workspaces::asking_for_person`]) has nothing
//! to compare. This is the host's own entry for such a turn. It is reached
//! from no transport: only the code that runs the turn calls it, with the
//! very caller the turn's tools run as and what the workspace's record of
//! its agents says of the agent ([`HostedTurn`]).
//!
//! The rule is the vault's own, and nothing here hands a secret to whoever
//! asks. A reference becomes its secret only inside this host, only in the
//! vault of the person the turn acts for, only for the kind the use needs
//! and only at the places the use sends it to, with the use written down
//! first ([`accounts::vault_items::VaultItems::use_as`]). The caller is
//! answered what the person would be: the account, the provider.
//!
//! Before the vault is opened a turn is refused unless all of this holds:
//!
//! - the caller is an agent a person's session was handed to, working
//!   inside a turn, confined to this workspace. A bot, an app, an agent
//!   admitted as a member and a person asking for themselves are not turns
//!   this host runs;
//! - the workspace admits that person now, on the device the caller names:
//!   a person removed from it leaves their agent nothing;
//! - the agent's keeper is that person. Whoever else writes to the agent
//!   makes its turn act for them, and then it uses nothing: not its
//!   keeper's vault, which is not theirs, and not their own, which the
//!   agent is not kept for;
//! - an agent whose record narrows it (a sub-agent) holds a rule that names
//!   exactly this use.
//!
//! A turn the host runs is unattended. So a use runs unasked only where the
//! item keeps a standing leave for this agent, this use and these places
//! ([`accounts::vault_items::VaultItems::stands`]). Otherwise the host keeps
//! what was asked ([`OpenAsk`]: the action and its arguments, which hold the
//! reference and no secret) and answers that the person must be asked
//! ([`TurnUse::Asks`]). Whoever runs the turn puts the question to them and
//! reads their own answer back ([`Workspaces::answer_use`]). What runs then
//! is what this host kept under the ask, never what a record says was
//! asked: a question an agent wrote itself has nothing kept here and runs
//! nothing, and one that shows something else than this host asked is
//! refused. An answer is carried out at most once.
//!
//! Another use by reference (a key added to a web call, a variable handed
//! to a command, a page's password field) is one more arm of [`Use`]: what
//! it wants of the vault and how it is carried out. The refusals, the
//! asking and the leave are the same for every use.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use accounts::vault_items::{self, ItemId, Place, VaultItems};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tilo_action_policy::{ActionKind, Answer, PersonId, Rule, TreePath};
use tilo_actions::remote::person_accounts::{self, AddMailWithItem};
use tilo_actions::remote::person_models::{self, SetKeyFromItem};
use tilo_actions::remote::person_vault::{self, ItemRef};
use tilo_actions::remote::{
    ActionHostError, Caller, CatalogueRow, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_lock::MutexRecover as _;
use tilo_relay_protocol::WorkspaceId;

use super::Workspaces;

/// Where a person's asks are kept, under their own root.
const ASKS_DIR: &str = "vault-asks";
/// The most asks that wait for one person at a time: an agent that goes on
/// asking is told so instead of filling their conversation.
const MAX_OPEN_ASKS: usize = 32;

/// What [`Workspaces::answer_use`] refuses a second reader of one answer
/// with: the first is carrying it out, or did.
pub const CARRIED_OUT: &str = "That answer was carried out already";
/// What [`Workspaces::answer_use`] refuses an ask this host keeps nothing
/// under with.
pub const NOTHING_WAITS: &str = "Nothing waits for an answer under that question";

/// A turn this host runs for a person, as the code that runs it knows it.
#[derive(Clone, Copy, Debug)]
pub struct HostedTurn<'a> {
    /// The workspace the turn works in.
    pub workspace: WorkspaceId,
    /// The caller the turn's tools run as: the person's session, handed to
    /// the agent on this turn.
    pub caller: &'a Caller,
    /// The agent's keeper, as the workspace's record of its agents names
    /// them.
    pub keeper: &'a str,
    /// What that record narrows the agent to; `None` follows the default.
    pub bound: Option<&'a [Rule]>,
}

/// How a use a hosted turn asked for went.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnUse {
    /// It was carried out: what its action answers, never a secret.
    Done(Value),
    /// Its person is asked first, and this host keeps what was asked.
    Asks(Box<OpenAsk>),
    /// Its person was asked the same before and has not answered yet.
    Waits,
}

/// What a person answered about a use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UseAnswer {
    /// Carry it out this once.
    Once,
    /// Carry it out, and from now on without asking: this agent, this use,
    /// this item, these places.
    Always,
    /// Do not carry it out.
    No,
}

/// A use this host keeps while its person is asked about it. Nothing in it
/// is a secret: its arguments name the vault item by reference.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenAsk {
    /// The ask's own id, which the question carries.
    pub ask: String,
    /// The workspace it was asked in.
    pub workspace: String,
    /// The person asked: their root key, hex.
    pub person: String,
    /// The agent that asked, by its id.
    pub agent: String,
    /// The name its person knows it under.
    pub name: String,
    /// Why it says it needs the item, in its own words; empty when it gave
    /// none.
    pub reason: String,
    /// Where whoever runs the turn put the question: theirs to read, kept
    /// here so it is found again after a restart.
    pub asked_in: Value,
    /// What the person is shown: the question's detail, exactly as it must
    /// stand when they answer.
    pub detail: Value,
    /// Whether an answer is being carried out: said before it starts, so
    /// an answer is carried out at most once whatever interrupts this host.
    pub started: bool,
    /// Whether this host stopped while an answer was being carried out: the
    /// ask was read back started from its disk, and nobody carries it out
    /// now.
    #[serde(skip)]
    pub interrupted: bool,
    /// When it was asked, in nanoseconds since the Unix epoch.
    pub asked_at_ns: u64,
    provider: String,
    resource: String,
    action: String,
    arguments: Map<String, Value>,
}

impl OpenAsk {
    fn target(&self) -> RemoteActionTarget {
        RemoteActionTarget {
            provider: self.provider.clone(),
            address: tilo_actions::ActionAddress::new(self.resource.clone(), self.action.clone()),
        }
    }
}

/// What uses of a vault item by reference keep on this host: what proves a
/// sign-in at its server before an account is kept, and the asks that wait
/// for their person's answer, read from the host's disk once.
pub(super) struct Uses {
    pub(super) prover: std::sync::RwLock<std::sync::Arc<dyn tilo_account_probe::AccountTester>>,
    asks: Mutex<Option<Vec<OpenAsk>>>,
}

impl Default for Uses {
    fn default() -> Self {
        Self {
            prover: std::sync::RwLock::new(std::sync::Arc::new(
                tilo_account_probe::LiveAccountTester,
            )),
            asks: Mutex::default(),
        }
    }
}

/// Whom a hosted turn uses the vault for, and as whom.
struct Using {
    person: [u8; 32],
    workspace: String,
    agent: String,
    name: String,
}

/// One use by reference this host carries out.
enum Use {
    /// A mail account added with the sign-in an item keeps.
    Mail(AddMailWithItem),
    /// A provider's key kept from an item.
    Key(SetKeyFromItem),
}

impl Use {
    /// The use `target` names with `arguments`. The reason never quotes the
    /// arguments.
    fn read(
        target: &RemoteActionTarget,
        arguments: &Map<String, Value>,
    ) -> Result<Self, ActionHostError> {
        let invalid = |what: &str| ActionHostError::InvalidRequest {
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
            _ => Err(ActionHostError::UnknownAction),
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
                let server = |scheme: &str, host: &str, port: u16| {
                    Place::server(scheme, host, port).map_err(|error| error.to_string())
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
        }
    }

    /// Why whoever asks says it needs the item.
    fn reason(&self) -> &str {
        match self {
            Self::Mail(adding) => adding.reason.trim(),
            Self::Key(setting) => setting.reason.trim(),
        }
    }

    /// Carry it out for `by` with what the vault under `root` keeps: the
    /// one place the reference becomes its secret, inside the code that
    /// signs in.
    async fn run(self, workspaces: &Workspaces, root: &Path, by: &str) -> Result<Value, String> {
        match self {
            Self::Mail(adding) => {
                let kept = super::person_vault::mail_with_item(
                    workspaces.account_tester(),
                    root,
                    adding,
                    by,
                )
                .await?;
                workspaces.person_settings_changed();
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
            Self::Key(setting) => {
                let kept = super::person_vault::key_from_item(root, &setting, by)?;
                serde_json::to_value(kept).map_err(|error| error.to_string())
            }
        }
    }
}

fn refused(reason: impl Into<String>) -> ActionHostError {
    ActionHostError::Refused {
        reason: reason.into(),
    }
}

/// The arguments `request` carries.
fn arguments_of(request: &InvokeRequest) -> Map<String, Value> {
    match &request.argument {
        InvokeArgument::Json(arguments) => arguments.clone(),
        _ => Map::new(),
    }
}

/// Whether `ask` is an ask's id: what names a file here.
fn is_ask_id(ask: &str) -> bool {
    ask.len() == 32
        && ask
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn ask_path(root: &Path, ask: &str) -> PathBuf {
    root.join(ASKS_DIR).join(format!("{ask}.json"))
}

/// Keep `ask` under its person's `root`, readable by this host alone.
fn keep(root: &Path, ask: &OpenAsk) -> Result<(), String> {
    let bytes = serde_json::to_vec(ask).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &ask_path(root, &ask.ask),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// The ask kept at `path`. One that does not read is said and left out: it
/// runs nothing, and its question is answered as no longer kept.
fn read_kept(path: &Path) -> Option<OpenAsk> {
    let read = std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
    match read {
        Ok(ask) => Some(ask),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "a kept ask about a vault use does not read");
            None
        }
    }
}

/// Every ask kept under the people's roots in `people`.
fn kept_under(people: &Path) -> Vec<OpenAsk> {
    let mut asks: Vec<OpenAsk> = std::fs::read_dir(people)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|person| {
            std::fs::read_dir(person.path().join(ASKS_DIR))
                .into_iter()
                .flatten()
        })
        .flatten()
        .filter_map(|file| read_kept(&file.path()))
        .filter(|ask: &OpenAsk| is_ask_id(&ask.ask))
        .map(|mut ask| {
            ask.interrupted = ask.started;
            ask
        })
        .collect();
    asks.sort_by(|a, b| {
        a.asked_at_ns
            .cmp(&b.asked_at_ns)
            .then_with(|| a.ask.cmp(&b.ask))
    });
    asks
}

impl Workspaces {
    /// The asks this host keeps, read from its disk the first time.
    fn hosted_asks(&self) -> MutexGuard<'_, Option<Vec<OpenAsk>>> {
        let mut asks = self.inner.vault_uses.asks.lock_recover();
        if asks.is_none() {
            *asks = Some(kept_under(&self.inner.dir.join("people")));
        }
        asks
    }

    /// Forget the ask `ask` of `person`: it runs nothing any more.
    fn forget_ask(&self, person: &str, ask: &str) {
        if let Some(asks) = self.hosted_asks().as_mut() {
            asks.retain(|kept| kept.ask != ask);
        }
        let mut root = [0; 32];
        if is_ask_id(ask) && hex::decode_to_slice(person, &mut root).is_ok() {
            let _ = std::fs::remove_file(ask_path(&self.person_settings_root(&root), ask));
        }
    }

    /// Whom `turn` uses the vault for when it asks for the use `target`
    /// names, or the refusal this module's rule gives it.
    async fn turn_uses_for(
        &self,
        turn: &HostedTurn<'_>,
        target: &RemoteActionTarget,
    ) -> Result<Using, ActionHostError> {
        let caller = turn.caller;
        let acting = caller.acting();
        // An agent a person's own session was handed to, inside a turn:
        // nothing else is a turn this host runs. Unknown and forbidden are
        // the same answer.
        if acting.app().is_some()
            || !acting.is_agent()
            || !caller.is_session_delegated()
            || caller.turn().is_none()
        {
            return Err(ActionHostError::UnknownAction);
        }
        let (Some(person), Some(device), Some(agent), Some(workspace)) = (
            caller.user_public_key().copied(),
            caller.device_public_key().copied(),
            acting.agent_id(),
            caller.workspace(),
        ) else {
            return Err(ActionHostError::UnknownAction);
        };
        if WorkspaceId::parse(workspace) != Some(turn.workspace) {
            return Err(ActionHostError::UnknownAction);
        }
        // The person's standing here now, on the device the turn names: a
        // person the workspace no longer admits leaves their agent nothing.
        if self.person_device(turn.workspace, person).await != Some(device) {
            return Err(ActionHostError::UnknownAction);
        }
        // The agent acts for its keeper and for nobody else.
        let Ok(keeper) = PersonId::parse(turn.keeper) else {
            return Err(ActionHostError::UnknownAction);
        };
        if keeper.root() != Some(person) {
            return Err(ActionHostError::UnknownAction);
        }
        // An agent its record narrows uses an item only where a rule of its
        // bound names this very use.
        if let Some(bound) = turn.bound {
            let node = TreePath::workspace(workspace)
                .app(target.provider.clone())
                .kind(target.address.resource.clone())
                .resource(target.address.resource.clone())
                .action(target.address.action.clone());
            let granted = tilo_automation::grant_answer(bound, &node, ActionKind::Outside);
            if !granted.names_action || granted.answer == Answer::Deny {
                return Err(ActionHostError::NotPermitted);
            }
        }
        Ok(Using {
            person,
            workspace: workspace.to_owned(),
            agent: agent.to_owned(),
            name: caller.agent_name().unwrap_or(agent).to_owned(),
        })
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

    /// Take the use by reference `request` asks for, for `turn`.
    ///
    /// Carried out at once where the item keeps a standing leave for this
    /// agent, this use and these places. Otherwise nothing is used: the host
    /// keeps what was asked, with `asked_in` (where the caller will put the
    /// question, its own to read), and answers [`TurnUse::Asks`] with what
    /// to show the person. A caller that cannot put the question says so
    /// ([`Self::use_not_asked`]).
    ///
    /// # Errors
    /// The turn is none this host runs for the item's person
    /// ([`ActionHostError::UnknownAction`]), its agent is narrowed and holds
    /// no rule for this use ([`ActionHostError::NotPermitted`]), the
    /// arguments do not read, or the vault refuses the use: no such item in
    /// that person's vault, another kind, a place it does not belong to.
    pub async fn use_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
        asked_in: Value,
    ) -> Result<TurnUse, ActionHostError> {
        let using = self.turn_uses_for(turn, &request.target).await?;
        let arguments = arguments_of(request);
        let wanted = Use::read(&request.target, &arguments)?;
        let root = self.person_settings_root(&using.person);
        let (item, kind, places) = wanted.wanted(&root).map_err(refused)?;
        let word = person_vault::use_word(&request.target);
        // What the use would be refused for is said before anyone is asked,
        // in the vault's own words; the vault is closed again before
        // anything is waited for.
        let stands = {
            let items = VaultItems::open_in(&root).map_err(|error| refused(error.to_string()))?;
            items
                .fits(&item, kind, &places)
                .and_then(|_| items.stands(&item, &places, &using.agent, &word))
                .map_err(|error| refused(error.to_string()))?
        };
        if stands {
            return wanted
                .run(self, &root, &using.name)
                .await
                .map(TurnUse::Done)
                .map_err(refused);
        }
        let belongs = places.iter().map(ToString::to_string).collect();
        self.ask_first(
            &root,
            using,
            (request, arguments),
            (kind, belongs),
            (wanted.reason().to_owned(), asked_in),
        )
    }

    /// Keep what `using` asked for with `request` until its person answers:
    /// the ask, and what to show them. `shown` is the kind of the item and
    /// the places the use would send its secret to.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the ask is looked for and kept under one lock, so the same use asked twice at once is kept once"
    )]
    fn ask_first(
        &self,
        root: &Path,
        using: Using,
        (request, arguments): (&InvokeRequest, Map<String, Value>),
        (kind, belongs): (vault_items::ItemKind, Vec<String>),
        (reason, asked_in): (String, Value),
    ) -> Result<TurnUse, ActionHostError> {
        let person = hex::encode(using.person);
        let mut asks = self.hosted_asks();
        let open = asks.get_or_insert_with(Vec::new);
        // Asked the same again while its person has not answered: the one
        // question stands.
        if open.iter().any(|kept| {
            kept.person == person
                && kept.agent == using.agent
                && kept.workspace == using.workspace
                && kept.target() == request.target
                && kept.arguments == arguments
        }) {
            return Ok(TurnUse::Waits);
        }
        if open.iter().filter(|kept| kept.person == person).count() >= MAX_OPEN_ASKS {
            return Err(refused(
                "Too many questions already wait for this person's answer",
            ));
        }
        let Some(described) = person_vault::use_of(&request.target) else {
            return Err(ActionHostError::UnknownAction);
        };
        let asked_at_ns = tilo_clock::unix_now_ns();
        let ask: String = blake3::hash(
            format!(
                "{person}\0{}\0{:?}\0{asked_at_ns}",
                using.agent, request.invocation
            )
            .as_bytes(),
        )
        .to_hex()
        .chars()
        .take(32)
        .collect();
        let detail = json!({
            "draft": tilo_actions::Draft::render(&described, &arguments),
            "use": {
                "ask": ask,
                "item": {"kind": kind, "belongsTo": belongs},
            },
        });
        let kept = OpenAsk {
            ask,
            workspace: using.workspace,
            person,
            agent: using.agent,
            name: using.name,
            reason,
            asked_in,
            detail,
            started: false,
            interrupted: false,
            asked_at_ns,
            provider: request.target.provider.clone(),
            resource: request.target.address.resource.clone(),
            action: request.target.address.action.clone(),
            arguments,
        };
        keep(root, &kept).map_err(refused)?;
        open.push(kept.clone());
        Ok(TurnUse::Asks(Box::new(kept)))
    }

    /// The question about `ask` could not be put to its person: nothing
    /// waits for an answer, and the use is asked for again when it is
    /// needed.
    pub fn use_not_asked(&self, ask: &OpenAsk) {
        self.forget_ask(&ask.person, &ask.ask);
    }

    /// The asks kept for `workspace`, oldest first: what whoever runs its
    /// agents reads answers for. Cheap where none waits.
    #[must_use]
    pub fn open_use_asks(&self, workspace: &str) -> Vec<OpenAsk> {
        self.hosted_asks()
            .as_ref()
            .map(|asks| {
                asks.iter()
                    .filter(|ask| ask.workspace == workspace)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// An answer to `ask` was being carried out when this host stopped:
    /// nobody carries it out now, and it is not carried out a second time.
    pub fn use_interrupted(&self, ask: &OpenAsk) {
        if ask.interrupted {
            self.forget_ask(&ask.person, &ask.ask);
        }
    }

    /// Carry out the person's answer to the ask `ask`: `shown` is the
    /// detail of the question as it stands in the record they answered,
    /// and `turn` is the asking agent's standing as this host reads it now.
    ///
    /// The caller read the answer from the person's own record of it; this
    /// host decides what it lets run. What runs is the use kept under the
    /// ask, held to every refusal of [`Self::use_for_turn`] again, for the
    /// person and the agent that asked. [`UseAnswer::Always`] writes the
    /// item's standing leave first. Answers what the use answered, `None`
    /// after [`UseAnswer::No`]. Whatever comes of it, the ask is spent.
    ///
    /// # Errors
    /// Nothing is kept under that ask, an answer to it was carried out
    /// already, the question shows something else than this host asked,
    /// the turn is not the asker's or is refused now, or the vault refuses
    /// the use.
    pub async fn answer_use(
        &self,
        turn: &HostedTurn<'_>,
        ask: &str,
        shown: &Value,
        answer: UseAnswer,
    ) -> Result<Option<Value>, ActionHostError> {
        let claimed = self.claim_use(ask)?;
        self.carry_out_use(turn, &claimed, shown, answer).await
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it
    /// out. It is said in this host's memory and on its disk before
    /// anything else is said or run, so two readers of one answer carry it
    /// out once, and a host that stops finds it started. Whoever claimed
    /// it carries it out ([`Self::carry_out_use`]).
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), another reader
    /// claimed it ([`CARRIED_OUT`]), or the claim could not be written down.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the claim is read and written under one lock, so two readers of one answer cannot both take it"
    )]
    pub fn claim_use(&self, ask: &str) -> Result<OpenAsk, ActionHostError> {
        let claimed = {
            let mut asks = self.hosted_asks();
            let Some(kept) = asks
                .as_mut()
                .and_then(|asks| asks.iter_mut().find(|kept| kept.ask == ask))
            else {
                return Err(refused(NOTHING_WAITS));
            };
            if kept.started {
                return Err(refused(CARRIED_OUT));
            }
            kept.started = true;
            kept.clone()
        };
        let mut person = [0; 32];
        let said = hex::decode_to_slice(&claimed.person, &mut person)
            .map_err(|error| error.to_string())
            .and_then(|()| keep(&self.person_settings_root(&person), &claimed));
        if let Err(error) = said {
            self.forget_ask(&claimed.person, &claimed.ask);
            return Err(refused(error));
        }
        Ok(claimed)
    }

    /// Carry out the answer to the ask `claimed` ([`Self::claim_use`]), as
    /// [`Self::answer_use`] says. Whatever comes of it, the ask is spent.
    ///
    /// # Errors
    /// As [`Self::answer_use`].
    pub async fn carry_out_use(
        &self,
        turn: &HostedTurn<'_>,
        claimed: &OpenAsk,
        shown: &Value,
        answer: UseAnswer,
    ) -> Result<Option<Value>, ActionHostError> {
        let carried = self.carry_out(turn, claimed, shown, answer).await;
        self.forget_ask(&claimed.person, &claimed.ask);
        carried
    }

    /// [`Self::answer_use`] for the ask it claimed.
    async fn carry_out(
        &self,
        turn: &HostedTurn<'_>,
        kept: &OpenAsk,
        shown: &Value,
        answer: UseAnswer,
    ) -> Result<Option<Value>, ActionHostError> {
        let mut person = [0; 32];
        hex::decode_to_slice(&kept.person, &mut person)
            .map_err(|_| ActionHostError::UnknownAction)?;
        let root = self.person_settings_root(&person);
        if kept.detail != *shown {
            return Err(refused(
                "The question is not the one this host asked: nothing was used",
            ));
        }
        let target = kept.target();
        let using = self.turn_uses_for(turn, &target).await?;
        if using.person != person || using.agent != kept.agent || using.workspace != kept.workspace
        {
            return Err(ActionHostError::UnknownAction);
        }
        if answer == UseAnswer::No {
            return Ok(None);
        }
        let wanted = Use::read(&target, &kept.arguments)?;
        if answer == UseAnswer::Always {
            let (item, kind, places) = wanted.wanted(&root).map_err(refused)?;
            let mut items =
                VaultItems::open_in(&root).map_err(|error| refused(error.to_string()))?;
            // A leave is given only for a use the item allows.
            items
                .fits(&item, kind, &places)
                .and_then(|_| {
                    items.let_always(
                        &item,
                        &places,
                        (&using.agent, &using.name),
                        &person_vault::use_word(&target),
                    )
                })
                .map_err(|error| refused(error.to_string()))?;
        }
        wanted
            .run(self, &root, &using.name)
            .await
            .map(Some)
            .map_err(refused)
    }
}
