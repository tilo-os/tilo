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
//! The password is its person's to type where no agent reads it. So the call
//! waits for its person's yes, showing them what is asked for in this
//! server's words and naming the vault's own add as what they answer it with
//! ([`tilo_actions::remote::asked::AskedFacts::takes`]): what they type goes
//! from their device to their vault, and never through the record. Their
//! approve admits the same call again, which signs in to the calendar server
//! with the login they kept for it and keeps the account for reading only
//! ([`super::add_with_item`]; nothing here can ask for anything else) in the
//! workspace's store, which is what its person was told their typing does.
//!
//! A calendar server that does not take the sign-in keeps nothing here: no
//! account; the login they typed stays in their vault, theirs to remove.
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
//! words.

use accounts::vault_items::Place;
use serde::{Deserialize, Serialize};
use tilo_actions::remote::asked::{AskedFacts, Fact};
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::person_vault::{self, AskForItem, ItemKind};
use tilo_actions::remote::{
    ActionCallError as ActionError, CatalogueRow, InvokeRequest, RemoteActionTarget,
};
use tilo_dav::Url;

use super::{AddWithItem, CONNECT, RESOURCE, account_id, accounts_in, argument, server_of};
use crate::connections::wire::{
    Destinations, Lookup, check_destination, is_public, system_lookup, written_out,
};
use crate::workspaces::Workspaces;
use crate::workspaces::hosted_use::{AskedTurn, HostedTurn};
use crate::workspaces::workspace_stores::Store;

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
const NO_ITEM: &str =
    "Your vault keeps no login for that calendar server, so nothing was connected";

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

/// What the vault keeps a calendar's password as: a login at the calendar server's own
/// origin `place`, under `username`.
fn login(place: &str, username: &str) -> AskForItem {
    AskForItem {
        kind: ItemKind::Login,
        belongs_to: vec![place.to_owned()],
        username: username.to_owned(),
        sent_in: String::new(),
    }
}

/// A calendar account as it was connected: what the agent that asked is
/// told, and nothing else of what the calendar server answered.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
    let literal = written_out(&url);
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

impl Workspaces {
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
    pub(crate) async fn calendar_connect_offered(
        &self,
        turn: &HostedTurn<'_>,
    ) -> Vec<CatalogueRow> {
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

    /// Connect the calendar `request` names for `turn`, once its person typed its password
    /// into their vault and approved this very call; until then it waits for their yes, and no
    /// calendar server is asked anything.
    ///
    /// # Errors
    /// The turn is none this server carries out
    /// ([`ActionError::UnknownAction`]); its agent is narrowed and holds no
    /// rule for this ([`ActionError::NotPermitted`]); the arguments are not
    /// an address, a user name and a name alone; the address is not https,
    /// carries a sign-in or is not on the public internet; the account is
    /// connected already; it waits for the person; their vault keeps no login for it; or the
    /// calendar server did not take the sign-in, in its own words.
    pub(crate) async fn connect_calendar_for_turn(
        &self,
        turn: &HostedTurn<'_>,
        request: &InvokeRequest,
    ) -> Result<Connected, ActionError> {
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
        let id = account_id(&username, &server);
        let held = self
            .workspace_accounts(&using.workspace)
            .await
            .map_err(refused)?;
        if accounts_in(&held).iter().any(|kept| kept.id == id) {
            return Err(refused(ALREADY));
        }
        let wanted = login(place.as_str(), &username);
        crate::workspaces::asking::approved_or_asks(request, || AskedFacts {
            words: format!(
                "Connect {username} at {} for reading only. {} never sees what you type: your \
                 vault keeps it for that server alone.",
                place.as_str(),
                using.name
            ),
            on: Some(account.clone()),
            facts: vec![
                Fact::new("kind", "login"),
                Fact::new("belongsTo", place.as_str()),
                Fact::new("username", username.clone()),
            ],
            verb: "connect".to_owned(),
            answered_by: Some(format!(
                "{}/{}#{}",
                person_vault::PROVIDER,
                person_vault::RESOURCE,
                person_vault::ADD
            )),
            ..AskedFacts::by(crate::workspaces::asking::asked_by(turn.caller))
        })?;
        let password = self
            .asked_item(&using.workspace, &wanted)
            .await
            .map_err(refused)?
            .ok_or_else(|| refused(NO_ITEM))?;
        let root = self.workspace_root(&using.workspace);
        let adding = AddWithItem {
            server_url: server.to_string(),
            username: Some(username),
            password,
            name: Some(account),
            // Always: nothing a turn hands over says otherwise.
            read_only: true,
        };
        let view = self
            .calendar_with_item(
                (&using.workspace, &root),
                adding,
                (&using.name, &using.acting(), Some(request)),
            )
            .await
            .map_err(refused)?;
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
    use std::net::IpAddr;
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
        for non_http in [
            "blob:http://127.0.0.1/x",
            "redis://127.0.0.1/",
            "file://127.0.0.1/etc/passwd",
        ] {
            assert_eq!(
                bounded(non_http, public, &never).await,
                Err(NOT_HTTPS),
                "{non_http} (public)"
            );
            assert_eq!(
                bounded(non_http, any, &never).await,
                Err(NOT_HTTPS),
                "{non_http} (any)"
            );
        }
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
