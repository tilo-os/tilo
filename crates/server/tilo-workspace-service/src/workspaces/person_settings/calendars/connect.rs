//! A workspace's calendar, connected by a person telling their assistant
//! where it is.
//!
//! A person connects anything by saying so to their assistant. A turn this
//! server carries out for them, with every device of theirs off, hands over
//! what they said: the calendar server's address, the user name there and a
//! name for the account ([`super::CONNECT`]). It hands over nothing else.
//! There is no argument for a password, none for a vault item and none that
//! says how the account is connected: an unknown argument is refused.
//!
//! The password is its person's to type where no agent reads it. So this
//! entry keeps what was asked ([`ConnectAsk`]) and answers that the person
//! must be asked ([`TurnConnect::Asks`]): whoever carries out the turn puts
//! a question of the kind that shows a protected field into the person's
//! own conversation with the agent, in the words this server says
//! ([`ConnectAsk::words`]). What they type goes from their device to their
//! vault, and the question is answered with the item's reference. Reading
//! that answer, the server carries out what it kept
//! ([`Workspaces::carry_out_connect`]), never what a conversation says was
//! asked: it signs in to the calendar server with what the vault item keeps,
//! keeps the account for reading only ([`super::add_with_item`]; nothing
//! here can ask for anything else) in the workspace's store, which is what
//! its person was told their typing does. An answer
//! is carried out at most once ([`Workspaces::claim_connect`]).
//!
//! A calendar server that does not take the sign-in keeps nothing here: no
//! account, and the item the person typed for this question is taken out of
//! the workspace's vault again: a login for that server alone that the vault
//! did not keep when it was asked. An item kept before the question stays.
//!
//! The address comes from a model's tool call, so it is held to what a
//! key's web call is held to ([`crate::workspaces::hosted_use`]) before
//! anybody is asked: https, no sign-in in it, and on the public internet,
//! both as the address says it and as its name answers when it is looked
//! up. Only where the server's operator lets it call its own network is an
//! address there taken. The connection that signs in later looks the name
//! up again, so a name that answers differently then is not caught here.
//!
//! The turn is one this server carries out ([`Workspaces::asked_turn`]), whose
//! agent the workspace's policy lets change its calendars, or it is offered nothing and
//! reaches nothing, and an agent that anything narrows
//! holds a rule that names this very action ([`HostedTurn::may_take`]). The
//! same account told twice adds nothing: connected already, it is refused in
//! words; asked for and not yet answered, the one question stands.

use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::MutexGuard;

use accounts::vault_items::{ItemId, ItemKind, Place};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
};
use tilo_dav::Url;
use tilo_lock::MutexRecover as _;

use super::{AddWithItem, CONNECT, RESOURCE, account_id, accounts_in, argument, server_of};
use crate::connections::wire::{Destinations, Lookup, check_destination, is_public, system_lookup};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{
    AskedSigner, AskedTurn, CARRIED_OUT, HostedTurn, NOTHING_WAITS,
};
use crate::workspaces::person_asking::workspace_store_root;
use crate::workspaces::workspace_stores::Store;

/// Where a workspace's connections that wait for a person's password are
/// kept, under its store.
const ASKS_DIR: &str = "calendar-asks";
/// The most that wait for one person at a time: an agent that goes on
/// asking is told so instead of filling their conversation.
const MAX_OPEN: usize = 8;
/// The longest user name and account name taken, in bytes.
const MAX_NAME_BYTES: usize = 320;
/// The longest name of a calendar told on, in characters, and how many
/// calendars are told of.
const MAX_CALENDAR_CHARS: usize = 80;
const MAX_CALENDARS: usize = 40;

/// What a telling's arguments are, as a refusal of them says it.
const WHAT: &str = "a calendar to connect, which names its server (its https address, its bare \
                    name, or its person's mail address there), the user name there and a name \
                    for the account and nothing else,";
const NOT_AN_ADDRESS: &str = "That is not the address of a calendar server";
const NOT_HTTPS: &str = "A calendar is connected only at an https address";
const CARRIES_SIGN_IN: &str = "The server's address must not carry a sign-in: its person types \
                               the password where no agent reads it";
const NOT_PUBLIC: &str = "That address is not on the public internet. A calendar is connected \
                          only at a public https address";
const NO_SUCH_SERVER: &str = "No server was found under that name";
const NOT_A_NAME: &str = "A calendar account has a user name, and a name of a few words";
const ALREADY: &str = "That calendar account is connected already. Nothing was added";
const TOO_MANY: &str = "Too many calendars already wait for this person's password";
const NO_ITEM: &str = "the answer names no item of the vault";
const NOT_THEIRS: &str = "the agent that asked no longer acts for this person here";

/// What a turn hands over to connect a calendar: what its person said, and
/// nothing that is a secret or that makes the account writable.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Connect {
    server_url: String,
    username: String,
    #[serde(default)]
    name: Option<String>,
}

/// A calendar a turn asked to connect, kept while its person is asked for
/// the password. Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectAsk {
    /// The ask's own id.
    pub ask: String,
    /// The workspace it was asked in.
    pub workspace: String,
    /// The person asked: their root key, hex.
    pub person: String,
    /// The agent that asked, by its id.
    pub agent: String,
    /// The roster signing session that asked for this sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) signer: Option<AskedSigner>,
    /// The name its person knows that agent under.
    pub name: String,
    /// The calendar server's address, as it was held to its bounds.
    pub server_url: String,
    /// Where the password belongs: the calendar server's own origin.
    pub place: String,
    /// The name the account signs in with there.
    pub username: String,
    /// What its person calls the account.
    pub account: String,
    /// The id the account is kept under once it is connected.
    pub account_id: String,
    /// Where whoever carries out the turn put the question: theirs to read,
    /// kept here so it is found again after a restart.
    pub asked_in: Value,
    /// Whether an answer is being carried out: said before it starts, so an
    /// answer is carried out at most once whatever interrupts this server.
    pub started: bool,
    /// Whether this server stopped while an answer was being carried out:
    /// the ask was read back started from its disk, and nobody carries it
    /// out now.
    #[serde(skip)]
    pub interrupted: bool,
    /// When it was asked, in milliseconds since the Unix epoch.
    pub asked_at_ms: u64,
    /// The items their vault kept when it was asked, by their ids: what
    /// is typed for this question is none of them.
    #[serde(default)]
    pub known: Vec<String>,
}

impl ConnectAsk {
    /// What the question says, in this server's own words: what is asked
    /// for and what typing it does.
    #[must_use]
    pub fn words(&self) -> String {
        format!(
            "Connect {} at {} for reading only. {} never sees what you type: your vault keeps \
             it for that server alone.",
            self.username, self.place, self.name
        )
    }

    /// What the vault keeps the password as: a login at the calendar
    /// server's own origin, under the user name.
    #[must_use]
    pub fn login(&self) -> tilo_actions::remote::person_vault::AskForItem {
        tilo_actions::remote::person_vault::AskForItem {
            kind: tilo_actions::remote::person_vault::ItemKind::Login,
            belongs_to: vec![self.place.clone()],
            username: self.username.clone(),
            sent_in: String::new(),
        }
    }

    /// What was asked for, in a few words of this server's own.
    #[must_use]
    pub fn named(&self) -> String {
        format!(
            "Connect the calendar account {} at {} for reading only",
            self.account, self.place
        )
    }
}

/// How a turn's asking to connect a calendar went.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnConnect {
    /// Its person is asked for the password first, and this server keeps
    /// what was asked.
    Asks(Box<ConnectAsk>),
    /// Its person was asked for this account's password before and has not
    /// answered yet.
    Waits,
}

/// A calendar account as it was connected: what the agent that asked is
/// told, and nothing else of what the calendar server answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Connected {
    /// The id the account is kept under.
    pub account: String,
    /// What its person calls it.
    pub name: String,
    /// The calendars the server lists, by their names: each on one line and
    /// cut where it is long, since a calendar's name is its server's to say.
    pub calendars: Vec<String>,
}

fn refused(reason: impl Into<String>) -> ActionError {
    ActionError::Refused {
        reason: reason.into(),
    }
}

/// The literal address `url` names, where it names one and not a name.
fn literal(url: &Url) -> Option<IpAddr> {
    if url.domain().is_some() {
        return None;
    }
    // What is left of an origin without its scheme: `127.0.0.1:8080`,
    // `[::1]:8080`, or the address alone on the scheme's own port.
    let origin = url.origin().ascii_serialization();
    let at = origin.split_once("://").map_or("", |(_, at)| at);
    let address = at.strip_prefix('[').map_or_else(
        || at.rsplit_once(':').map_or(at, |(address, _)| address),
        |inside| inside.split(']').next().unwrap_or_default(),
    );
    tilo_failure::ignored!(
        "what is no literal address is a name, and a name is looked up",
        address.parse::<IpAddr>().ok()
    )
}

/// `said` as an address written whole. What a person says is taken as they
/// say it: an address with its scheme stands as it is; a server's bare name,
/// with the port and the path said with it, is that name's https address; a
/// mail address is the https address of the name it ends in, where a
/// calendar is looked for first. Nothing else is an address.
fn written_whole(said: &str) -> Option<String> {
    if said.contains("://") {
        return Some(said.to_owned());
    }
    if said.is_empty()
        || said
            .chars()
            .any(|sign| sign.is_whitespace() || sign.is_control())
    {
        return None;
    }
    let name = match said.split_once('@') {
        None => said,
        Some((who, name)) => {
            let named = !name.is_empty()
                && name
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.');
            (named && !who.is_empty() && !who.contains(['@', ':', '/'])).then_some(name)?
        }
    };
    Some(format!("https://{name}"))
}

/// The address a turn named, when a calendar is connected there by a server
/// that reaches `reach` and looks a name up with `lookup`: https, carrying
/// no sign-in, and on the public internet as the address says it and as
/// every address its name answers with. Under [`Destinations::Any`] an
/// address inside is taken too, and plain http to a literal one there: a
/// development server on this machine.
async fn bounded(address: &str, reach: Destinations, lookup: &Lookup) -> Result<Url, &'static str> {
    let written = written_whole(address.trim()).ok_or(NOT_AN_ADDRESS)?;
    let url = Url::parse(&written).map_err(|_| NOT_AN_ADDRESS)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(CARRIES_SIGN_IN);
    }
    let literal = literal(&url);
    let plain_inside = url.scheme() == "http"
        && reach == Destinations::Any
        && literal.is_some_and(|address| !is_public(address));
    if url.scheme() != "https" && !plain_inside {
        return Err(NOT_HTTPS);
    }
    check_destination(&url, reach).map_err(|_| NOT_PUBLIC)?;
    if reach == Destinations::Public && literal.is_none() {
        let name = url.domain().ok_or(NOT_AN_ADDRESS)?.to_owned();
        let answers = lookup(name).await.map_err(|_| NO_SUCH_SERVER)?;
        if answers.is_empty() {
            return Err(NO_SUCH_SERVER);
        }
        if !answers.into_iter().all(is_public) {
            return Err(NOT_PUBLIC);
        }
    }
    Ok(url)
}

/// `name` as an account's user name or its shown name is taken: trimmed, on
/// one line and short.
fn a_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// A calendar's `name` as it is told on: on one line and cut where it is
/// long.
fn told_name(name: &str) -> String {
    let line = name
        .split(|character: char| character.is_control() || character.is_whitespace())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    line.chars().take(MAX_CALENDAR_CHARS).collect()
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

/// Keep `ask` under its workspace's `root`, readable by this server alone.
fn keep(root: &Path, ask: &ConnectAsk) -> Result<(), String> {
    let bytes = serde_json::to_vec(ask).map_err(|error| error.to_string())?;
    tilo_files::native::atomic::write_creating_parent_with_mode(
        &ask_path(root, &ask.ask),
        &bytes,
        0o600,
    )
    .map_err(|error| error.to_string())
}

/// The ask kept at `path`. One that does not read is said and left out: it
/// connects nothing.
fn read_kept(path: &Path) -> Option<ConnectAsk> {
    let read = std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
    match read {
        Ok(ask) => Some(ask),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "a kept ask to connect a calendar does not read");
            None
        }
    }
}

/// Every ask kept under the workspaces' stores in `stores`, oldest first.
/// One that was started is read back interrupted.
fn kept_under(stores: &Path) -> Vec<ConnectAsk> {
    let mut asks: Vec<ConnectAsk> = std::fs::read_dir(stores)
        .into_iter()
        .flatten()
        .flatten()
        .flat_map(|workspace| {
            std::fs::read_dir(workspace.path().join(ASKS_DIR))
                .into_iter()
                .flatten()
        })
        .flatten()
        .filter_map(|file| read_kept(&file.path()))
        .filter(|ask: &ConnectAsk| is_ask_id(&ask.ask))
        .map(|mut ask| {
            ask.interrupted = ask.started;
            ask
        })
        .collect();
    asks.sort_by(|a, b| {
        a.asked_at_ms
            .cmp(&b.asked_at_ms)
            .then_with(|| a.ask.cmp(&b.ask))
    });
    asks
}

/// The item `item` names in the workspace's vault, when it is the one the
/// protected field of `ask`'s question made: a login that belongs to the
/// calendar server's origin and to nothing else, which their vault did not
/// keep when it was asked. Such an item is taken out again when nothing is
/// connected with it; any other stays.
async fn typed_for(workspaces: &Workspaces, item: &ItemRef, ask: &ConnectAsk) -> Option<ItemId> {
    let id = ItemId::parse(item.id())?;
    let facts = workspaces
        .open_vault(&ask.workspace)
        .await
        .ok()?
        .items()
        .facts()
        .into_iter()
        .find(|facts| facts.id == id)?;
    let own = facts.kind == ItemKind::Login
        && facts.places.len() == 1
        && facts
            .places
            .first()
            .is_some_and(|place| place.as_str() == ask.place)
        && !ask.known.iter().any(|known| known == id.as_str());
    own.then_some(id)
}

/// Take the item `id` out of the workspace's vault again.
async fn take_out(workspaces: &Workspaces, workspace: &str, id: &ItemId) {
    let taken = workspaces
        .changing_vault(workspace, |items| Ok(items.remove(id)))
        .await;
    if let Err(error) = taken {
        tracing::warn!(%error, "an item typed for a calendar that was not connected stays in the workspace's vault");
    }
}

impl Workspaces {
    /// The asks to connect a calendar this server keeps, read from its disk
    /// the first time.
    fn connects(&self) -> MutexGuard<'_, Option<Vec<ConnectAsk>>> {
        let mut asks = self.inner.vault_uses.connects.lock_recover();
        if asks.is_none() {
            *asks = Some(kept_under(&workspace_store_root(&self.inner.dir)));
        }
        asks
    }

    /// Forget the ask `ask` of `workspace`: nothing is connected for it any
    /// more.
    fn forget_connect(&self, workspace: &str, ask: &str) {
        if let Some(asks) = self.connects().as_mut() {
            asks.retain(|kept| kept.ask != ask);
        }
        if is_ask_id(ask) {
            let _ = std::fs::remove_file(ask_path(&self.workspace_root(workspace), ask));
        }
    }

    /// Whose turn `turn` is, where it may ask `target`, the connecting of a
    /// calendar: one this server carries out, whose agent the workspace's policy lets change
    /// its calendars, and for an agent that anything narrows one a rule of
    /// its names.
    async fn connecting(
        &self,
        turn: &HostedTurn<'_>,
        target: &RemoteActionTarget,
    ) -> Result<AskedTurn, ActionError> {
        if target.provider != person_accounts::PROVIDER
            || target.address.resource != RESOURCE
            || target.address.action != CONNECT
        {
            return Err(ActionError::UnknownAction);
        }
        let using = self.asked_turn(turn).await?;
        if !self
            .turn_reaches(
                turn,
                Store::CALENDARS,
                CONNECT,
                tilo_action_policy::ActionKind::Undoable,
            )
            .await
        {
            return Err(ActionError::UnknownAction);
        }
        if !turn.may_take(&using.workspace, target) {
            return Err(ActionError::NotPermitted);
        }
        Ok(using)
    }

    /// The connecting of a calendar `turn` is offered, as a catalogue lists
    /// it: none for a turn this server does not carry out, and none for a
    /// narrowed agent no rule of which names it.
    pub async fn calendar_connect_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let row = Self::calendar_connect_row();
        let Some(action) = &row.action else {
            return Vec::new();
        };
        let target = RemoteActionTarget {
            provider: row.provider.clone(),
            address: action.address.clone(),
        };
        if self.connecting(turn, &target).await.is_ok() {
            vec![row]
        } else {
            Vec::new()
        }
    }

    /// Take what `request` says of a calendar to connect, for `turn`.
    ///
    /// Nothing is connected and no calendar server is asked anything: the
    /// server keeps what was asked, with `asked_in` (where the caller will
    /// put the question, its own to read), and answers
    /// [`TurnConnect::Asks`] with what to ask the person. A caller that
    /// cannot put the question says so ([`Self::connect_not_asked`]).
    ///
    /// # Errors
    /// The turn is none this server carries out
    /// ([`ActionError::UnknownAction`]); its agent is narrowed and holds no
    /// rule for this ([`ActionError::NotPermitted`]); the arguments are not
    /// an address, a user name and a name alone; the address is not https,
    /// carries a sign-in or is not on the public internet; the account is
    /// connected already; or too many such asks wait for the person.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the ask is looked for and kept under one lock, so the same account asked twice at once is kept once"
    )]
    pub async fn connect_calendar_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
        asked_in: Value,
    ) -> Result<TurnConnect, ActionError> {
        let using = self.connecting(turn, &request.target).await?;
        let asked: Connect = argument(request, WHAT)?;
        let username = a_name(&asked.username).ok_or_else(|| refused(NOT_A_NAME))?;
        let account = match asked.name.as_deref().map(str::trim) {
            None | Some("") => username.clone(),
            Some(name) => a_name(name).ok_or_else(|| refused(NOT_A_NAME))?,
        };
        let reach = if self.inner.models.private_addresses() {
            Destinations::Any
        } else {
            Destinations::Public
        };
        let address = bounded(&asked.server_url, reach, &system_lookup())
            .await
            .map_err(refused)?;
        // The one rule every calendar sign-in is under holds here too.
        let server = server_of(address.as_str()).map_err(refused)?;
        let place = Place::parse(server.as_str()).map_err(|error| refused(error.to_string()))?;
        let root = self.workspace_root(&using.workspace);
        let id = account_id(&username, &server);
        let held = self
            .workspace_accounts(&using.workspace)
            .await
            .map_err(refused)?;
        if accounts_in(&held).iter().any(|kept| kept.id == id) {
            return Err(refused(ALREADY));
        }
        // What the vault keeps now stays whatever comes of this.
        let known = self
            .open_vault(&using.workspace)
            .await
            .map_err(refused)?
            .items()
            .facts()
            .into_iter()
            .map(|facts| facts.id.as_str().to_owned())
            .collect();
        let person = hex::encode(using.person);
        let asked_at_ms = tilo_clock::unix_now_ms();
        let ask: String = blake3::hash(
            format!(
                "{person}\0{}\0{:?}\0{asked_at_ms}",
                using.agent, request.invocation
            )
            .as_bytes(),
        )
        .to_hex()
        .chars()
        .take(32)
        .collect();
        let kept = ConnectAsk {
            ask,
            workspace: using.workspace,
            person,
            agent: using.agent,
            signer: Some(using.signer),
            name: using.name,
            server_url: server.to_string(),
            place: place.as_str().to_owned(),
            username,
            account,
            account_id: id,
            asked_in,
            started: false,
            interrupted: false,
            asked_at_ms,
            known,
        };
        let mut asks = self.connects();
        let open = asks.get_or_insert_with(Vec::new);
        // Told of the same account again while its person has not answered:
        // the one question stands.
        if open.iter().any(|waits| {
            waits.person == kept.person
                && waits.workspace == kept.workspace
                && waits.signer == kept.signer
                && waits.account_id == kept.account_id
        }) {
            return Ok(TurnConnect::Waits);
        }
        if open
            .iter()
            .filter(|waits| waits.person == kept.person)
            .count()
            >= MAX_OPEN
        {
            return Err(refused(TOO_MANY));
        }
        keep(&root, &kept).map_err(refused)?;
        open.push(kept.clone());
        Ok(TurnConnect::Asks(Box::new(kept)))
    }

    /// The question about `ask` could not be put to its person, or it is
    /// over without an answer: nothing waits any more, and the calendar is
    /// asked for again when it is wanted.
    pub fn connect_not_asked(&self, ask: &ConnectAsk) {
        self.forget_connect(&ask.workspace, &ask.ask);
    }

    /// The asks to connect a calendar kept for `workspace`, oldest first:
    /// what whoever carries out its agents reads answers for. Cheap where
    /// none waits.
    #[must_use]
    pub fn open_connects(&self, workspace: &str) -> Vec<ConnectAsk> {
        self.connects()
            .as_ref()
            .map(|asks| {
                asks.iter()
                    .filter(|ask| ask.workspace == workspace)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it
    /// out. It is said in this server's memory and on its disk before
    /// anything else is said or done, so two readers of one answer carry it
    /// out once, and a server that stops finds it started. Whoever claimed
    /// it carries it out ([`Self::carry_out_connect`]).
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`NOTHING_WAITS`]), another reader
    /// claimed it ([`CARRIED_OUT`]), or the claim could not be written down.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the claim is read and written under one lock, so two readers of one answer cannot both take it"
    )]
    pub fn claim_connect(&self, ask: &str) -> Result<ConnectAsk, ActionError> {
        let claimed = {
            let mut asks = self.connects();
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
        let said = keep(&self.workspace_root(&claimed.workspace), &claimed);
        if let Err(error) = said {
            self.forget_connect(&claimed.workspace, &claimed.ask);
            return Err(refused(error));
        }
        Ok(claimed)
    }

    /// Carry out what the person answered where they were asked for the
    /// password of the calendar `claimed` keeps ([`Self::claim_connect`]).
    /// `typed` is the reference their device answered with, `None` where
    /// they declined. `turn` is the asking agent's turn as this server
    /// reads it now.
    ///
    /// What the person typed is in their vault already, under `typed`;
    /// this server decides what comes of it. What is connected is the
    /// account kept under the ask, for reading only, signed in with what
    /// the vault item keeps.
    /// Answers the account and the names of its calendars, `None` where
    /// they declined. Where nothing is connected the item typed for this
    /// question is taken out of their vault again. Whatever comes of it,
    /// the ask is spent.
    ///
    /// # Errors
    /// The turn is not the asker's or is refused now, the answer names no item the vault
    /// hands to this calendar server, or the calendar server did not take
    /// the sign-in: in its own words.
    pub async fn carry_out_connect(
        &self,
        turn: &HostedTurn<'_>,
        claimed: &ConnectAsk,
        typed: Option<&str>,
    ) -> Result<Option<Connected>, String> {
        let root = self.workspace_root(&claimed.workspace);
        let item = typed.and_then(ItemRef::parse);
        let own = match &item {
            Some(item) => typed_for(self, item, claimed).await,
            None => None,
        };
        let carried = match typed {
            None => Ok(None),
            Some(_) => self.connect(turn, claimed, &root, item).await.map(Some),
        };
        if !matches!(carried, Ok(Some(_)))
            && let Some(id) = &own
        {
            take_out(self, &claimed.workspace, id).await;
        }
        self.forget_connect(&claimed.workspace, &claimed.ask);
        carried
    }

    /// Connect the account `kept` names with the sign-in `item` keeps in the
    /// vault under `root`, for reading only.
    async fn connect(
        &self,
        turn: &HostedTurn<'_>,
        kept: &ConnectAsk,
        root: &Path,
        item: Option<ItemRef>,
    ) -> Result<Connected, String> {
        let target = RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: tilo_actions::ActionAddress::new(RESOURCE, CONNECT),
        };
        let using = self
            .connecting(turn, &target)
            .await
            .map_err(|_| NOT_THEIRS.to_owned())?;
        if hex::encode(using.person) != kept.person
            || using.agent != kept.agent
            || using.workspace != kept.workspace
            || !AskedSigner::permits_turn(kept.signer.as_ref(), self, turn, Store::CALENDARS.app)
                .await
        {
            return Err(NOT_THEIRS.to_owned());
        }
        let password = item.ok_or_else(|| NO_ITEM.to_owned())?;
        let adding = AddWithItem {
            server_url: kept.server_url.clone(),
            username: Some(kept.username.clone()),
            password,
            name: Some(kept.account.clone()),
            // Always: nothing a turn hands over says otherwise.
            read_only: true,
        };
        let view = self
            .calendar_with_item((&kept.workspace, root), adding, &using.name)
            .await?;
        self.person_settings_changed();
        Ok(Connected {
            account: view.id,
            name: view.name,
            calendars: view
                .calendars
                .iter()
                .take(MAX_CALENDARS)
                .map(|calendar| told_name(&calendar.name))
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use std::future::Future;
    use std::pin::Pin;
    use std::sync::Arc;

    use super::*;

    /// A lookup that answers every name with `answers`; none is no such
    /// name.
    fn answering(answers: &'static [&'static str]) -> Lookup {
        Arc::new(move |_name: String| {
            let found: std::io::Result<Vec<IpAddr>> = if answers.is_empty() {
                Err(std::io::Error::other("no such name"))
            } else {
                Ok(answers
                    .iter()
                    .filter_map(|address| address.parse().ok())
                    .collect())
            };
            Box::pin(async move { found })
                as Pin<Box<dyn Future<Output = std::io::Result<Vec<IpAddr>>> + Send>>
        })
    }

    /// A name is taken only where every address it answers with is on the
    /// public internet: one answer inside is enough to refuse it, wherever
    /// it stands among the others, and a name nobody answers for is none.
    #[tokio::test]
    async fn a_name_is_taken_only_where_every_address_it_answers_with_is_public() {
        let named = "https://calendar.example.test/dav/";
        let public = Destinations::Public;
        for inside in [
            &["127.0.0.1"][..],
            &["10.0.0.8"],
            &["192.168.1.4"],
            &["169.254.169.254"],
            &["::1"],
            &["fd00::8"],
            &["93.184.216.34", "10.0.0.8"],
            &["10.0.0.8", "93.184.216.34"],
        ] {
            assert_eq!(
                bounded(named, public, &answering(inside)).await,
                Err(NOT_PUBLIC),
                "{inside:?}"
            );
        }
        assert_eq!(
            bounded(named, public, &answering(&[])).await,
            Err(NO_SUCH_SERVER)
        );
        let taken = bounded(named, public, &answering(&["93.184.216.34"]))
            .await
            .unwrap();
        assert_eq!(taken.as_str(), named);
    }

    /// What an address says is judged before any name is looked up: not
    /// https, a sign-in in it, a literal address inside. Only a server whose
    /// operator lets it call its own network takes an address there, and
    /// plain http only to a literal one.
    #[tokio::test]
    async fn an_address_is_https_carries_no_sign_in_and_is_not_inside() {
        let never = answering(&[]);
        let public = Destinations::Public;
        for (address, why) in [
            ("http://calendar.example.test/dav/", NOT_HTTPS),
            ("ftp://calendar.example.test/dav/", NOT_HTTPS),
            (
                "https://ada:made-up@calendar.example.test/",
                CARRIES_SIGN_IN,
            ),
            ("https://ada@calendar.example.test/", CARRIES_SIGN_IN),
            ("https://127.0.0.1:8443/dav/", NOT_PUBLIC),
            ("https://10.0.0.8/dav/", NOT_PUBLIC),
            ("https://[::1]/dav/", NOT_PUBLIC),
            ("https://[fd00::8]/dav/", NOT_PUBLIC),
            ("http://127.0.0.1:8080/dav/", NOT_HTTPS),
            ("", NOT_AN_ADDRESS),
        ] {
            assert_eq!(
                bounded(address, public, &never).await,
                Err(why),
                "{address}"
            );
        }
        let any = Destinations::Any;
        assert!(
            bounded("http://127.0.0.1:8080/dav/", any, &never)
                .await
                .is_ok()
        );
        assert!(bounded("https://10.0.0.8/dav/", any, &never).await.is_ok());
        assert_eq!(
            bounded("http://calendar.example.test/dav/", any, &never).await,
            Err(NOT_HTTPS),
            "plain http to a name, wherever the server may call"
        );
    }

    /// What a person says is taken as they say it: a server's bare name, with
    /// the port and the path they say with it, and their mail address, whose
    /// calendar is looked for under the name the address ends in. Each is
    /// the https address of that name, and is held to the public internet as
    /// an address written whole is.
    #[tokio::test]
    async fn a_bare_name_or_a_mail_address_is_the_https_address_of_that_name() {
        let public = Destinations::Public;
        let outside = answering(&["93.184.216.34"]);
        for (said, taken) in [
            ("calendar.example.test", "https://calendar.example.test/"),
            (
                " calendar.example.test/dav/ ",
                "https://calendar.example.test/dav/",
            ),
            (
                "calendar.example.test:8443/dav/",
                "https://calendar.example.test:8443/dav/",
            ),
            ("ada@example.test", "https://example.test/"),
        ] {
            assert_eq!(
                bounded(said, public, &outside).await.map(String::from),
                Ok(taken.to_owned()),
                "{said}"
            );
        }
        for said in ["calendar.example.test", "ada@example.test"] {
            assert_eq!(
                bounded(said, public, &answering(&["93.184.216.34", "10.0.0.8"])).await,
                Err(NOT_PUBLIC),
                "{said}: one answer inside refuses the name"
            );
            assert_eq!(
                bounded(said, public, &answering(&[])).await,
                Err(NO_SUCH_SERVER),
                "{said}: a name nobody answers for"
            );
        }
        for inside in ["10.0.0.8", "127.0.0.1:8443/dav/", "[::1]/dav/"] {
            assert_eq!(
                bounded(inside, public, &outside).await,
                Err(NOT_PUBLIC),
                "{inside}"
            );
        }
        for said in [
            "",
            "my calendar",
            "ada@",
            "@example.test",
            "ada@example.test/dav/",
            "ada:made-up@example.test",
            "ada@grace@example.test",
        ] {
            assert_eq!(
                bounded(said, public, &outside).await,
                Err(NOT_AN_ADDRESS),
                "{said}"
            );
        }
    }

    /// A calendar's name is its server's to say: it is told on one line and
    /// cut where it is long.
    #[test]
    fn a_calendars_name_is_told_on_one_line_and_cut_where_it_is_long() {
        assert_eq!(told_name("  Work \n calendar\t"), "Work calendar");
        assert_eq!(
            told_name(&"x".repeat(200)).chars().count(),
            MAX_CALENDAR_CHARS
        );
    }
}
