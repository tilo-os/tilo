//! A workspace's mail account, signed in by a person telling their assistant
//! its address.
//!
//! A person says "log in my mail account" and its address, and nothing
//! else. A turn this server carries out for them hands over that address
//! ([`CONNECT`]) and, only where they said them, the name the account signs
//! in with, a name for the account and its two servers. There is no argument
//! for a password and none for a vault item: an unknown argument is refused.
//!
//! This server works out where the mail of that address lives
//! ([`tilo_account_probe::AccountTester::find_mail_servers`]). Only a server
//! it may ask is asked anything ([`answers_in_public`]), and a provider
//! whose accounts sign in on a page of its own is refused in words before
//! anybody is asked. Then the call waits for its person's yes, its asked
//! record naming both servers and how they were found, with the vault's own
//! add as what answers it ([`tilo_actions::remote::asked::AskedFacts::takes`]):
//! what they type goes from their device to their vault, bound to those two
//! servers. Their approve admits the call again, which signs in with the
//! newest login their vault keeps for those servers and keeps the account in
//! the workspace's store. Neither the password nor the item's reference
//! passes a model. A server that does not take the password keeps nothing,
//! and the call ends with its refusal.
//!
//! The turn is one this server carries out ([`Workspaces::asked_turn`]), whose
//! agent the workspace's policy lets change its mail, or it is offered nothing and
//! reaches nothing, and an agent that anything narrows
//! holds a rule that names this very action ([`HostedTurn::may_take`]). An
//! account signed in already is refused in words.

use accounts::vault_items::{Place, UsedFor};
use serde::{Deserialize, Serialize};
use tilo_account_probe::mail_servers::{Found, Server};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_accounts::{self, AddMailWithItem};
use tilo_actions::remote::person_vault::{self, AskForItem, ItemKind};
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
};

use super::{answers_in_public, argument};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::workspace_stores::Store;

/// Sign a mail account in by a turn's telling, from its address alone.
pub const CONNECT: &str = "connect";

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
const NO_ITEM: &str = "your vault keeps no login for those two servers, so nothing was signed in";

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

/// A mail account a turn asks to sign in, as worked out from what it said.
/// Nothing in it is a secret.
#[derive(Clone, Debug, PartialEq)]
struct AskedAccount {
    /// The name its person knows the asking agent under.
    name: String,
    /// The account's mail address, which is also the id it is kept under.
    address: String,
    /// The name the account signs in with.
    username: String,
    /// The name mail from the account carries, where its person said one.
    account: String,
    /// The server its mail is read from (IMAP), and the one it is handed to (SMTP).
    reads: Server,
    sends: Server,
    /// How the servers were found, as a clause the question says.
    found: &'static str,
}

impl AskedAccount {
    /// Where the password belongs: the two servers, as a vault item names them.
    fn belongs_to(&self) -> Result<[String; 2], ActionError> {
        let place = |scheme: &str, server: &Server| {
            Place::server(scheme, &server.name, server.port)
                .map(|place| place.as_str().to_owned())
                .map_err(|error| refused(error.to_string()))
        };
        Ok([place("imap", &self.reads)?, place("smtp", &self.sends)?])
    }

    /// What its person is asked, in this server's own words: what is asked
    /// for, where it goes and what typing it does.
    fn words(&self) -> String {
        format!(
            "Sign in to {} at {} and {}, found {}. {} never sees what you type: your vault keeps \
             it for those two servers alone.",
            self.address, self.reads.name, self.sends.name, self.found, self.name
        )
    }

    /// What the vault keeps the password as: a login that belongs to both
    /// servers, under the name the account signs in with.
    fn login(&self, belongs_to: &[String; 2]) -> AskForItem {
        AskForItem {
            kind: ItemKind::Login,
            belongs_to: belongs_to.to_vec(),
            username: self.username.clone(),
            sent_in: String::new(),
        }
    }
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

impl Workspaces {
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
    pub(crate) async fn mail_connect_offered(&self, turn: &HostedTurn<'_>) -> Vec<CatalogueRow> {
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

    /// Sign in the mail account `request` names, for `turn`, once its person
    /// approved: its servers are worked out from the address, and the call
    /// waits for their yes and the password they type into their vault.
    ///
    /// # Errors
    /// The turn is none this server carries out
    /// ([`ActionError::UnknownAction`]); its agent is narrowed and holds no
    /// rule for this ([`ActionError::NotPermitted`]); nobody approved it yet
    /// ([`ActionError::ConfirmationRequired`]); the arguments are not an
    /// address and what its person said alone; the account is signed in
    /// already; its provider signs in on a page of its own; no server this
    /// server may ask was found for it; their vault keeps no login for those
    /// servers; or the sign-in failed, in its own words.
    pub(crate) async fn connect_mail_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<MailSignedIn, ActionError> {
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
        let servers = self
            .mail_servers_of(&address, named)
            .await
            .map_err(refused)?;
        let mail = AskedAccount {
            name: using.name.clone(),
            username: username
                .or(servers.username)
                .unwrap_or_else(|| address.clone()),
            address,
            account,
            reads: servers.reads,
            sends: servers.sends,
            found: servers.found,
        };
        let belongs_to = mail.belongs_to()?;
        crate::workspaces::asking::approved_or_asks(request, || AskedFacts {
            words: mail.words(),
            on: Some(mail.address.clone()),
            facts: vec![
                Fact::new("kind", "login"),
                Fact::new("belongsTo", belongs_to.join(" ")),
                Fact::new("username", mail.username.clone()),
            ],
            verb: "sign in".to_owned(),
            answered_by: Some(format!(
                "{}/{}#{}",
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD
            )),
            ..AskedFacts::by(crate::workspaces::asking::asked_by(turn.caller))
        })?;
        let password = self
            .asked_item(&using.workspace, &mail.login(&belongs_to))
            .await
            .map_err(refused)?
            .ok_or_else(|| refused(NO_ITEM))?;
        let root = self.workspace_root(&using.workspace);
        let adding = AddMailWithItem {
            address: mail.address.clone(),
            display_name: mail.account.clone(),
            username: Some(mail.username.clone()),
            incoming_host: mail.reads.name.clone(),
            incoming_port: mail.reads.port,
            outgoing_host: mail.sends.name.clone(),
            outgoing_port: mail.sends.port,
            password,
            reason: String::new(),
        };
        // No agent uses the item: this server signs in with it once, for the account its
        // person typed it for.
        let view = self
            .mail_with_item(
                (&using.workspace, &root),
                adding,
                (&using.name, &using.acting(), Some(request)),
                UsedFor::default(),
            )
            .await
            .map_err(refused)?;
        self.person_settings_changed();
        Ok(MailSignedIn {
            account: view.id,
            address: view.address,
            reads: mail.reads.name,
            sends: mail.sends.name,
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

    fn an_account() -> AskedAccount {
        AskedAccount {
            name: "Scout".to_owned(),
            address: "ada@example.test".to_owned(),
            username: "ada@example.test".to_owned(),
            account: String::new(),
            reads: Server::new("imap.provider.test", 993),
            sends: Server::new("smtp.provider.test", 465),
            found: how(Found::Published),
        }
    }

    /// The question names the account and both servers in a few words, and
    /// what answers it is a login that belongs to both.
    #[test]
    fn a_question_names_both_servers() {
        let mail = an_account();
        let words = mail.words();
        for part in [
            "ada@example.test",
            "imap.provider.test",
            "smtp.provider.test",
            "Scout",
            "never sees what you type",
        ] {
            assert!(words.contains(part), "{part}: {words}");
        }
        let login = mail.login(&mail.belongs_to().unwrap());
        assert_eq!(
            login.belongs_to,
            [
                "imap://imap.provider.test:993",
                "smtp://smtp.provider.test:465"
            ]
        );
        assert_eq!(login.username, "ada@example.test");
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
