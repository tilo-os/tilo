//! A workspace's mail account, signed in by a person telling their assistant
//! its address.
//!
//! A person says "log in my mail account" and its address, and nothing
//! else. A turn this server carries out for them, with every device of
//! theirs off, hands over that address ([`CONNECT`]) and, only where they
//! said them, the name the account signs in with, a name for the account and
//! its two servers. There is no argument for a password and none for a vault
//! item: an unknown argument is refused.
//!
//! This server works out where the mail of that address lives
//! ([`tilo_account_probe::AccountTester::find_mail_servers`]). Only a server
//! it may ask is asked anything ([`answers_in_public`]), and a provider
//! whose accounts sign in on a page of its own is refused in words before
//! anybody is asked. Then this entry keeps what was asked ([`MailAsk`]) and
//! answers that the person must be asked ([`TurnMailConnect::Asks`]):
//! whoever carries out the turn puts a question of the kind that shows a
//! protected field into the person's own conversation with the agent, in
//! this server's words ([`MailAsk::words`]), which name both servers and how
//! they were found. What they type goes from their device to their vault,
//! bound to those two servers, and the question is answered with the item's
//! reference. Reading that answer, the server carries out what it kept
//! ([`Workspaces::carry_out_mail_connect`]), never what a conversation says
//! was asked: it signs in with what the item keeps and keeps the account in
//! the workspace's store. Neither the password nor the item's
//! reference passes a model, and nobody is asked to allow a use: no agent
//! uses the item.
//!
//! A server that does not take the password keeps nothing here: no account,
//! and the item typed for the question is taken out of the vault again. The
//! person is then asked again, under a new ask for the same servers
//! ([`MailCarried::AsksAgain`]), as often as it takes: nothing here counts
//! their tries, and no question ends because time passed.
//!
//! The turn is one this server carries out ([`Workspaces::asked_turn`]), whose
//! agent the workspace's policy lets change its mail, or it is offered nothing and
//! reaches nothing, and an agent that anything narrows
//! holds a rule that names this very action ([`HostedTurn::may_take`]). The
//! same account told twice adds nothing: signed in already, it is refused in
//! words; asked for and not yet answered, the one question stays.

use std::path::Path;

use accounts::vault_items::{ItemId, ItemKind, Place, UsedFor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tilo_account_probe::mail_servers::{Found, Server};
use tilo_actions::remote::person_accounts::{self, AddMailWithItem};
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
};

use super::{answers_in_public, argument};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::person_asking::kept_asks::{KeptAskKind, hashed_ask_id};
use crate::workspaces::workspace_stores::Store;

/// Sign a mail account in by a turn's telling, from its address alone.
pub const CONNECT: &str = "connect";

/// Where a workspace's mail accounts that wait for a person's password are
/// kept, under its store.
const ASKS_DIR: &str = "mail-asks";
/// The longest user name, account name and server name taken, in bytes: each
/// comes from a model's tool call and is shown on one line.
const MAX_NAME_BYTES: usize = 320;
/// Where mail is read over a connection secured from its first byte, and
/// where it is handed in over one.
const IMAPS: u16 = 993;
const SUBMISSIONS: u16 = 465;

/// What a telling's arguments are, as a refusal of them says it.
const WHAT: &str = "a mail account to sign in to, which names its address and, only where its \
                    person said them, a user name, a name for the account and its two servers, \
                    and nothing else,";
const NOT_A_NAME: &str = "A user name, an account's name and a server's name are a few words on \
                          one line";
const ALREADY: &str = "That mail account is signed in already. Nothing was added";
const OWN_PAGE: &str = "signs its accounts in on a page of its own and takes no password from a \
                        mail program. That way of signing in is not offered on your person's \
                        server yet: tell them so. Nobody was asked for anything";
const NAME_THEM: &str = "Ask your person for the names of the two servers, the one its mail is \
                         read from (IMAP) and the one it is handed to (SMTP), and pass them as \
                         incomingServer and outgoingServer. Nobody was asked for a password";
const NOT_A_SERVER: &str = "That is not the name of a mail server. Write a server as its name, \
                            such as imap.example.org, and where it answers at another port \
                            than the usual one with that port after a colon, such as \
                            imap.example.org:1993";
const NOT_PUBLIC: &str = "is not on the public internet. A mail account is signed in to only at \
                          servers there";
const NOT_LOOKED: &str = "the look for its mail servers did not finish";
const NO_ITEM: &str = "the answer names no item of the vault";
const NOT_THEIRS: &str = "the agent that asked no longer acts for this person here";
/// What a mail server's refusal of a password holds, in the words of whatever
/// proves a sign-in ([`tilo_account_probe`]).
const REJECTED: &str = "rejected the saved credential";

/// What a turn hands over to sign a mail account in: what its person said,
/// and nothing that is a secret.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Connect {
    address: String,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    incoming_server: Option<String>,
    #[serde(default)]
    outgoing_server: Option<String>,
}

/// A mail account a turn asked to sign in, kept while its person is asked
/// for the password. Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MailAsk {
    /// The ask's own id.
    pub ask: String,
    /// The workspace it was asked in.
    pub workspace: String,
    /// The person asked: their root key, hex.
    pub person: String,
    /// The agent that asked, by its id: the one named for the account once
    /// it is signed in.
    pub agent: String,
    /// The name its person knows that agent under.
    pub name: String,
    /// The account's mail address, which is also the id it is kept under.
    pub address: String,
    /// The name the account signs in with.
    pub username: String,
    /// The name mail from the account carries, where its person said one.
    pub account: String,
    /// The server its mail is read from (IMAP), and its port.
    pub reads: String,
    pub reads_port: u16,
    /// The server its mail is handed to (SMTP), and its port.
    pub sends: String,
    pub sends_port: u16,
    /// Where the password belongs: those two servers, as a vault item names
    /// them.
    pub belongs_to: [String; 2],
    /// How the servers were found, as a clause the question says.
    pub found: String,
    /// The server that did not take the password typed before, where this
    /// ask follows such a refusal.
    #[serde(default)]
    pub refused_by: String,
    /// Which turn asked: whoever carries out the turn reads it, kept here so
    /// it is found again after a restart.
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
    /// The items their vault kept when it was asked, by their ids: what is
    /// typed for this question is none of them.
    #[serde(default)]
    pub known: Vec<String>,
}

impl MailAsk {
    /// What the question says, in this server's own words: what is asked
    /// for, where it goes and what typing it does.
    #[must_use]
    pub fn words(&self) -> String {
        let again = if self.refused_by.is_empty() {
            String::new()
        } else {
            format!("That password was not accepted by {}. ", self.refused_by)
        };
        format!(
            "{again}Sign in to {} at {} and {}. {} never sees what you type: your vault keeps \
             it for those two servers alone.",
            self.address, self.reads, self.sends, self.name
        )
    }

    /// What the vault keeps the password as: a login that belongs to both
    /// servers, under the name the account signs in with.
    #[must_use]
    pub fn login(&self) -> tilo_actions::remote::person_vault::AskForItem {
        tilo_actions::remote::person_vault::AskForItem {
            kind: tilo_actions::remote::person_vault::ItemKind::Login,
            belongs_to: self.belongs_to.to_vec(),
            username: self.username.clone(),
            sent_in: String::new(),
        }
    }

    /// What was asked for, in a few words of this server's own.
    #[must_use]
    pub fn named(&self) -> String {
        format!("Sign in to the mail account {}", self.address)
    }

    /// The two servers, as the agent that asked is told of them.
    #[must_use]
    pub fn servers(&self) -> String {
        format!(
            "its mail is read at {}:{} and sent through {}:{} ({})",
            self.reads, self.reads_port, self.sends, self.sends_port, self.found
        )
    }
}

impl KeptAskKind for MailAsk {
    type Error = ActionError;
    const DIR: &'static str = ASKS_DIR;
    const READ_FAILED: &'static str = "a kept ask to sign a mail account in does not read";
    // Deliberate: mail sign-in asks are deduplicated per (person, workspace, address)
    // and retries replace the spent ask rather than accumulating, with no open-ask cap.
    const MAX_OPEN: Option<(usize, &'static str)> = None;

    fn id(&self) -> &str {
        &self.ask
    }

    fn workspace(&self) -> &str {
        &self.workspace
    }

    fn person(&self) -> &str {
        &self.person
    }

    fn started(&self) -> bool {
        self.started
    }

    fn set_started(&mut self, started: bool) {
        self.started = started;
    }

    fn interrupted(&self) -> bool {
        self.interrupted
    }

    fn set_interrupted(&mut self, interrupted: bool) {
        self.interrupted = interrupted;
    }

    fn asked_at(&self) -> u64 {
        self.asked_at_ms
    }

    fn refused(reason: impl Into<String>) -> Self::Error {
        refused(reason)
    }
}

/// How a turn's asking to sign a mail account in went.
#[derive(Clone, Debug, PartialEq)]
pub enum TurnMailConnect {
    /// Its person is asked for the password first, and this server keeps
    /// what was asked.
    Asks(Box<MailAsk>),
    /// Its person was asked for this account's password before and has not
    /// answered yet.
    Waits,
}

/// A mail account as it was signed in: what the agent that asked is told.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MailSignedIn {
    /// The id the account is kept under.
    pub account: String,
    /// Its mail address.
    pub address: String,
    /// The server its mail is read from, and the one it is handed to.
    pub reads: String,
    pub sends: String,
}

/// What came of a person's answer where they were asked for a mail account's
/// password.
#[derive(Clone, Debug, PartialEq)]
pub enum MailCarried {
    /// The account is signed in and kept in the workspace.
    SignedIn(MailSignedIn),
    /// They declined to type it: nothing was signed in.
    Declined,
    /// A server did not take what they typed: nothing is kept of it, and
    /// they are asked again under this new ask, which names that server.
    AsksAgain(Box<MailAsk>),
}

/// The servers of one address, and how they were found.
struct Servers {
    reads: Server,
    sends: Server,
    found: &'static str,
    username: Option<String>,
}

fn refused(reason: impl Into<String>) -> ActionError {
    ActionError::Refused {
        reason: reason.into(),
    }
}

/// How servers were found, as a clause a question says after "I worked out
/// its servers from the address".
const fn how(found: Found) -> &'static str {
    match found {
        Found::Known => "as its provider is known to keep them",
        Found::Published => "as its provider publishes them for mail programs",
        Found::Service => "as its domain names them",
        Found::Names => "under their usual names, each with a valid certificate for its name",
    }
}
/// The same for servers its person named after nothing was found.
const AS_NAMED: &str = "as you named them";

/// `name` as an account's user name or its shown name is taken: trimmed, on
/// one line and short.
fn a_name(name: &str) -> Option<String> {
    let name = name.trim();
    (!name.is_empty() && name.len() <= MAX_NAME_BYTES && !name.chars().any(char::is_control))
        .then(|| name.to_owned())
}

/// What a turn said of one optional name: none where it said nothing.
fn said(name: Option<&str>) -> Result<Option<String>, ActionError> {
    match name.map(str::trim) {
        None | Some("") => Ok(None),
        Some(name) => a_name(name).map(Some).ok_or_else(|| refused(NOT_A_NAME)),
    }
}

/// Whether `name` is written as a server's name is: letters, digits, hyphens
/// and dots.
fn is_server_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME_BYTES
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.')
}

/// The server a turn named: its name alone, which is asked at the `usual`
/// port, or its name and, after a colon, the port it answers at. A person's
/// own server need not sit where its domain publishes nothing. None where
/// that is not how a server is written.
fn named_server(said: &str, usual: u16) -> Option<Server> {
    let Some((name, port)) = said.rsplit_once(':') else {
        return is_server_name(said).then(|| Server::new(said, usual));
    };
    // A port is written in digits alone, and none is 0.
    if port.is_empty() || !port.bytes().all(|digit| digit.is_ascii_digit()) {
        return None;
    }
    let Ok(port) = port.parse::<u16>() else {
        return None;
    };
    (port != 0 && is_server_name(name)).then(|| Server::new(name, port))
}

/// The server that refused the password, where `why` is a mail server's
/// refusal of one: the one mail is handed to when that one said so, else the
/// one it is read from, which is asked first.
fn refused_by(why: &str, ask: &MailAsk) -> Option<String> {
    why.contains(REJECTED).then(|| {
        if why.starts_with("SMTP") {
            ask.sends.clone()
        } else {
            ask.reads.clone()
        }
    })
}

/// The ids of every item the workspace's vault keeps now.
async fn kept_items(workspaces: &Workspaces, workspace: &str) -> Result<Vec<String>, String> {
    Ok(workspaces
        .open_vault(workspace)
        .await?
        .items()
        .facts()
        .into_iter()
        .map(|facts| facts.id.as_str().to_owned())
        .collect())
}

/// The item `item` names in the workspace's vault, when it is the one the
/// protected field of `ask`'s question made: a login that belongs to the
/// account's two servers and to nothing else, which their vault did not keep
/// when it was asked. Such an item is taken out again when nothing is signed
/// in with it; any other stays.
async fn typed_for(workspaces: &Workspaces, item: &ItemRef, ask: &MailAsk) -> Option<ItemId> {
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
        && facts.places.len() == ask.belongs_to.len()
        && facts.places.iter().all(|place| {
            ask.belongs_to
                .iter()
                .any(|belongs| belongs.as_str() == place.as_str())
        })
        && !ask.known.iter().any(|known| known == id.as_str());
    own.then_some(id)
}

/// Take the item `id` out of the workspace's vault again.
async fn take_out(workspaces: &Workspaces, workspace: &str, id: &ItemId) {
    let taken = workspaces
        .changing_vault(workspace, None, |items| Ok(items.remove(id)))
        .await;
    if let Err(error) = taken {
        tracing::warn!(%error, "an item typed for a mail account that was not signed in stays in the workspace's vault");
    }
}

impl Workspaces {
    /// Forget the ask `ask` of `workspace`: nothing is signed in for it any
    /// more.
    fn forget_mail_connect(&self, workspace: &str, ask: &str) {
        self.inner
            .vault_uses
            .mail_connects
            .forget(&self.inner.dir, workspace, ask);
    }

    /// Whose turn `turn` is, where it may ask `target`, the signing in of a
    /// mail account: one this server carries out, whose agent the workspace's policy lets
    /// change its mail, and for an agent that anything narrows one a rule of
    /// its names.
    async fn mail_connecting(
        &self,
        turn: &HostedTurn<'_>,
        target: &RemoteActionTarget,
    ) -> Result<AskedTurn, ActionError> {
        if target.provider != person_accounts::PROVIDER
            || target.address.resource != person_accounts::RESOURCE
            || target.address.action != CONNECT
        {
            return Err(ActionError::UnknownAction);
        }
        let using = self.asked_turn(turn).await?;
        if !self
            .turn_reaches(
                turn,
                Store::MAIL,
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

    /// The signing in of a mail account `turn` is offered, as a catalogue
    /// lists it: none for a turn this server does not carry out, and none for
    /// a narrowed agent no rule of which names it.
    pub async fn mail_connect_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
        let target = person_accounts::target(CONNECT);
        if self.mail_connecting(turn, &target).await.is_ok() {
            vec![Self::mail_connect_row()]
        } else {
            Vec::new()
        }
    }

    /// Where the mail of `address` lives, as whatever proves sign-ins here
    /// works it out; failing that, at the two servers its person `named`.
    /// Only a server every address of which is public is asked anything,
    /// except where this server's operator lets it call its own network.
    async fn mail_servers_of(
        &self,
        address: &str,
        named: Option<(String, String)>,
    ) -> Result<Servers, String> {
        let anywhere = self.inner.models.private_addresses();
        let tester = self.account_tester();
        let of = address.to_owned();
        tokio::task::spawn_blocking(move || {
            let allowed = move |name: &str| anywhere || answers_in_public(name);
            match tester.find_mail_servers(&of, &allowed) {
                Ok(found) => match found.own_sign_in {
                    Some(provider) => Err(format!("{provider} {OWN_PAGE}")),
                    None => Ok(Servers {
                        found: how(found.found),
                        reads: found.incoming,
                        sends: found.outgoing,
                        username: found.username,
                    }),
                },
                Err(why) => {
                    let Some((reads, sends)) = named else {
                        return Err(format!("{why}. {NAME_THEM}"));
                    };
                    let (Some(reads), Some(sends)) = (
                        named_server(&reads, IMAPS),
                        named_server(&sends, SUBMISSIONS),
                    ) else {
                        return Err(NOT_A_SERVER.to_owned());
                    };
                    for server in [&reads, &sends] {
                        if !allowed(&server.name) {
                            return Err(format!("{} {NOT_PUBLIC}", server.name));
                        }
                    }
                    Ok(Servers {
                        reads,
                        sends,
                        found: AS_NAMED,
                        username: None,
                    })
                }
            }
        })
        .await
        .map_err(|_| NOT_LOOKED.to_owned())?
    }

    /// Take what `request` says of a mail account to sign in, for `turn`.
    ///
    /// Nothing is signed in: the account's servers are worked out, the
    /// server keeps what was asked, with `asked_in` (which turn asked, the
    /// caller's own to read), and answers [`TurnMailConnect::Asks`] with what
    /// to ask the person. A caller that cannot ask says so
    /// ([`Self::mail_connect_not_asked`]).
    ///
    /// # Errors
    /// The turn is none this server carries out
    /// ([`ActionError::UnknownAction`]); its agent is narrowed and holds no
    /// rule for this ([`ActionError::NotPermitted`]); the arguments are not
    /// an address and what its person said alone; the account is signed in
    /// already; its provider signs in on a page of its own; or no server
    /// this server may ask was found for it.
    pub async fn connect_mail_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
        asked_in: Value,
    ) -> Result<TurnMailConnect, ActionError> {
        let using = self.mail_connecting(turn, &request.target).await?;
        let asked: Connect = argument(request, WHAT)?;
        let address = asked.address.trim().to_owned();
        let username = said(asked.username.as_deref())?;
        let account = said(asked.name.as_deref())?.unwrap_or_default();
        let named =
            said(asked.incoming_server.as_deref())?.zip(said(asked.outgoing_server.as_deref())?);
        if self
            .workspace_accounts(&using.workspace)
            .await
            .map_err(refused)?
            .account(&address)
            .is_some()
        {
            return Err(refused(ALREADY));
        }
        let person = hex::encode(using.person);
        // Told of the same account again while its person has not answered:
        // the one question stays, and no server is asked anything.
        if self
            .inner
            .vault_uses
            .mail_connects
            .any(&self.inner.dir, |waits| {
                waits.person == person
                    && waits.workspace == using.workspace
                    && waits.address == address
            })
        {
            return Ok(TurnMailConnect::Waits);
        }
        let servers = self
            .mail_servers_of(&address, named)
            .await
            .map_err(refused)?;
        let place = |scheme: &str, server: &Server| {
            Place::server(scheme, &server.name, server.port)
                .map(|place| place.as_str().to_owned())
                .map_err(|error| refused(error.to_string()))
        };
        let belongs_to = [
            place("imap", &servers.reads)?,
            place("smtp", &servers.sends)?,
        ];
        // What their vault keeps now is theirs whatever comes of this.
        let known = kept_items(self, &using.workspace).await.map_err(refused)?;
        let kept = self.inner.vault_uses.mail_connects.keep(
            &self.inner.dir,
            &person,
            (using, address),
            |waits, (using, address)| {
                waits.person == person
                    && waits.workspace == using.workspace
                    && waits.address == *address
            },
            |(using, address)| {
                let asked_at_ms = tilo_clock::unix_now_ms();
                let ask = hashed_ask_id(
                    &person,
                    &using.agent,
                    &format!("{:?}", request.invocation),
                    asked_at_ms,
                );
                Ok(MailAsk {
                    ask,
                    workspace: using.workspace,
                    person: person.clone(),
                    agent: using.agent,
                    name: using.name,
                    username: username
                        .or(servers.username)
                        .unwrap_or_else(|| address.clone()),
                    address,
                    account,
                    reads: servers.reads.name,
                    reads_port: servers.reads.port,
                    sends: servers.sends.name,
                    sends_port: servers.sends.port,
                    belongs_to,
                    found: servers.found.to_owned(),
                    refused_by: String::new(),
                    asked_in,
                    started: false,
                    interrupted: false,
                    asked_at_ms,
                    known,
                })
            },
        )?;
        Ok(match kept {
            Some(kept) => TurnMailConnect::Asks(Box::new(kept)),
            None => TurnMailConnect::Waits,
        })
    }

    /// The question about `ask` could not be put to its person, or it is
    /// over without an answer: nothing waits any more, and the account is
    /// asked for again when it is wanted.
    pub fn mail_connect_not_asked(&self, ask: &MailAsk) {
        self.forget_mail_connect(&ask.workspace, &ask.ask);
    }

    /// The asks to sign a mail account in kept for `workspace`, oldest
    /// first: what whoever carries out its agents reads answers for. Cheap
    /// where none waits.
    #[must_use]
    pub fn open_mail_connects(&self, workspace: &str) -> Vec<MailAsk> {
        self.inner
            .vault_uses
            .mail_connects
            .open(&self.inner.dir, workspace)
    }

    /// Claim the ask `ask` for the one reader of its answer that carries it
    /// out. It is said in this server's memory and on its disk before
    /// anything else is said or done, so two readers of one answer carry it
    /// out once, and a server that stops finds it started. Whoever claimed
    /// it carries it out ([`Self::carry_out_mail_connect`]).
    ///
    /// # Errors
    /// Nothing is kept under that ask ([`crate::workspaces::hosted_use::NOTHING_WAITS`]), another
    /// reader claimed it ([`crate::workspaces::hosted_use::CARRIED_OUT`]), or the claim could not
    /// be written down.
    pub fn claim_mail_connect(&self, ask: &str) -> Result<MailAsk, ActionError> {
        self.inner
            .vault_uses
            .mail_connects
            .claim(&self.inner.dir, ask)
    }

    /// Carry out what the person answered where they were asked for the
    /// password of the mail account `claimed` keeps
    /// ([`Self::claim_mail_connect`]). `typed` is the reference their device
    /// answered with, `None` where they declined. `turn` is the asking
    /// agent's turn as this server reads it now.
    ///
    /// What the person typed is in their vault already, under `typed`;
    /// this server decides what comes of it. What is signed in is the
    /// account kept under the ask, at the servers kept under it, with what
    /// the vault item keeps. Where nothing is signed in the item typed for
    /// this question is taken out of the vault again. Whatever comes of it, the ask is spent; a
    /// server that did not take the password leaves a new one
    /// ([`MailCarried::AsksAgain`]), for the caller to ask.
    ///
    /// # Errors
    /// The turn is not the asker's or is refused now, the answer names no item the vault
    /// hands to these servers, or the sign-in failed for another reason
    /// than its password: in its own words.
    pub async fn carry_out_mail_connect(
        &self,
        turn: &HostedTurn<'_>,
        claimed: &MailAsk,
        typed: Option<&str>,
    ) -> Result<MailCarried, String> {
        let root = self.workspace_root(&claimed.workspace);
        let item = typed.and_then(ItemRef::parse);
        let own = match &item {
            Some(item) => typed_for(self, item, claimed).await,
            None => None,
        };
        let carried = match typed {
            None => Ok(None),
            Some(_) => self
                .sign_in_mail(turn, claimed, &root, item)
                .await
                .map(Some),
        };
        if !matches!(carried, Ok(Some(_)))
            && let Some(id) = &own
        {
            take_out(self, &claimed.workspace, id).await;
        }
        self.forget_mail_connect(&claimed.workspace, &claimed.ask);
        match carried {
            Ok(Some(signed_in)) => Ok(MailCarried::SignedIn(signed_in)),
            Ok(None) => Ok(MailCarried::Declined),
            Err(why) => match refused_by(&why, claimed) {
                Some(server) => self
                    .ask_mail_again(claimed, server)
                    .await
                    .map(|again| MailCarried::AsksAgain(Box::new(again))),
                None => Err(why),
            },
        }
    }

    /// Keep a new ask for the account `spent` kept, after `server` did not
    /// take the password typed for it: the same servers, and the items the
    /// vault keeps now.
    async fn ask_mail_again(&self, spent: &MailAsk, server: String) -> Result<MailAsk, String> {
        let asked_at_ms = tilo_clock::unix_now_ms();
        let again = MailAsk {
            ask: hashed_ask_id(&spent.person, &spent.agent, &spent.ask, asked_at_ms),
            refused_by: server,
            started: false,
            interrupted: false,
            asked_at_ms,
            known: kept_items(self, &spent.workspace).await?,
            ..spent.clone()
        };
        self.inner
            .vault_uses
            .mail_connects
            .insert(&self.inner.dir, again)
    }

    /// Sign the account `kept` names in with what `item` keeps in the vault
    /// under `root`, and keep it.
    async fn sign_in_mail(
        &self,
        turn: &HostedTurn<'_>,
        kept: &MailAsk,
        root: &Path,
        item: Option<ItemRef>,
    ) -> Result<MailSignedIn, String> {
        let using = self
            .mail_connecting(turn, &person_accounts::target(CONNECT))
            .await
            .map_err(|_| NOT_THEIRS.to_owned())?;
        if hex::encode(using.person) != kept.person
            || using.agent != kept.agent
            || using.workspace != kept.workspace
        {
            return Err(NOT_THEIRS.to_owned());
        }
        let password = item.ok_or_else(|| NO_ITEM.to_owned())?;
        let adding = AddMailWithItem {
            address: kept.address.clone(),
            display_name: kept.account.clone(),
            username: Some(kept.username.clone()),
            incoming_host: kept.reads.clone(),
            incoming_port: kept.reads_port,
            outgoing_host: kept.sends.clone(),
            outgoing_port: kept.sends_port,
            password,
            reason: String::new(),
        };
        // No agent uses the item: this server signs in with it once, for the account its
        // person typed it for.
        let view = self
            .mail_with_item(
                (&kept.workspace, root),
                adding,
                &using.name,
                UsedFor::default(),
            )
            .await?;
        self.person_settings_changed();
        Ok(MailSignedIn {
            account: view.id,
            address: view.address,
            reads: kept.reads.clone(),
            sends: kept.sends.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn an_ask() -> MailAsk {
        MailAsk {
            ask: "0".repeat(32),
            workspace: "w".to_owned(),
            person: "p".to_owned(),
            agent: "scout".to_owned(),
            name: "Scout".to_owned(),
            address: "ada@example.test".to_owned(),
            username: "ada@example.test".to_owned(),
            account: String::new(),
            reads: "imap.provider.test".to_owned(),
            reads_port: 993,
            sends: "smtp.provider.test".to_owned(),
            sends_port: 465,
            belongs_to: [
                "imap://imap.provider.test:993".to_owned(),
                "smtp://smtp.provider.test:465".to_owned(),
            ],
            found: how(Found::Published).to_owned(),
            refused_by: String::new(),
            asked_in: Value::Null,
            started: false,
            interrupted: false,
            asked_at_ms: 1,
            known: Vec::new(),
        }
    }

    /// The question names the account and both servers in a few words that fit the
    /// protected field's prompt, and its field is a login that belongs to both; asked again, it
    /// says first which server did not take the password.
    #[test]
    fn a_question_names_both_servers_and_after_a_refusal_which_one_refused() {
        let ask = an_ask();
        let words = ask.words();
        for part in [
            "ada@example.test",
            "imap.provider.test",
            "smtp.provider.test",
            "Scout",
            "never sees what you type",
        ] {
            assert!(words.contains(part), "{part}: {words}");
        }
        let login = ask.login();
        assert_eq!(
            login.belongs_to,
            [
                "imap://imap.provider.test:993",
                "smtp://smtp.provider.test:465"
            ]
        );
        assert_eq!(login.username, "ada@example.test");
        let again = MailAsk {
            refused_by: "imap.provider.test".to_owned(),
            ..ask
        };
        assert!(
            again
                .words()
                .starts_with("That password was not accepted by imap.provider.test. "),
            "{}",
            again.words()
        );
    }

    /// A mail server's refusal of a password names the server it came from;
    /// any other failure of a sign-in asks nobody again.
    #[test]
    fn only_a_refused_password_is_asked_for_again_and_names_its_server() {
        let ask = an_ask();
        assert_eq!(
            refused_by("IMAP server rejected the saved credential", &ask).as_deref(),
            Some("imap.provider.test")
        );
        assert_eq!(
            refused_by("SMTP server rejected the saved credential", &ask).as_deref(),
            Some("smtp.provider.test")
        );
        assert_eq!(refused_by("connection refused", &ask), None);
    }

    /// A server is asked only where every address its name answers with is
    /// public: this machine itself is not, under its name or its address,
    /// and what is written as no server's name is none.
    #[test]
    fn a_server_inside_is_not_one_that_answers_in_public() {
        assert!(!answers_in_public("localhost"));
        assert!(!answers_in_public("127.0.0.1"));
        assert!(!answers_in_public("10.0.0.8"));
        assert!(is_server_name("imap.provider.test"));
        assert!(!is_server_name("imap.provider.test/inbox"));
        assert!(!is_server_name("ada@provider.test"));
        assert!(!is_server_name(""));
    }

    /// A server is named by its name, and asked at the usual port, or by its
    /// name and the port it answers at; what is written any other way is no
    /// server, and nothing is asked.
    #[test]
    fn a_server_named_with_its_port_is_taken_at_that_port() {
        assert_eq!(
            named_server("imap.provider.test", IMAPS),
            Some(Server::new("imap.provider.test", 993))
        );
        assert_eq!(
            named_server("Mail.Provider.test:1993", IMAPS),
            Some(Server::new("mail.provider.test", 1993))
        );
        assert_eq!(
            named_server("smtp.provider.test:587", SUBMISSIONS),
            Some(Server::new("smtp.provider.test", 587))
        );
        for no_server in [
            "",
            ":993",
            "imap.provider.test:",
            "imap.provider.test:0",
            "imap.provider.test:65536",
            "imap.provider.test:+993",
            "imap.provider.test:99 3",
            "imap.provider.test:imap",
            "imap.provider.test:993:993",
            "[::1]:993",
            "imap.provider.test/inbox:993",
            "ada@provider.test:993",
        ] {
            assert_eq!(named_server(no_server, IMAPS), None, "{no_server}");
        }
    }
}
