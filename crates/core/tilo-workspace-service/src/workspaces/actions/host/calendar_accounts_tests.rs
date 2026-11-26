//! A calendar account at a `CalDAV` server its person names, connected through the workspace's
//! accounts (`tilo.accounts#add-calendar`), as Settings and every agent ask it. A bare server
//! name is that server over https; an address that would send the sign-in in the clear, or that
//! carries one, is refused before any server is asked. The server is asked for its calendars
//! with the password typed, and one that refuses the sign-in keeps nothing here. An account
//! connected for reading only, and every calendar of it, stays so whoever adds it again.
//!
//! An agent's connect waits for its person, with the password in nothing they are shown: their
//! no connects nothing and asks no server anything, the same connect asked again waits as it
//! was, and once the account is connected an agent's connect of it adds nothing and asks nobody.
//! Another member's agent connects for the whole workspace once its own person allowed it. An
//! app acting for nobody connects nothing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, and each test keeps its host to the end"
)]

use std::sync::{Arc, Mutex};

use crate::workspace_accounts;
use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_actions::remote::asked::Offer;
use tilo_actions::remote::{ActionCallError, Caller, person_vault};
use tilo_dav::discovery::CalendarCollection;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, hosted};
use super::turn_use_tests::asks;
use super::vault_tests::ask_at;
use super::waiting::answered_by;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost;
use crate::workspaces::tests::{hosting_device, test_host};

/// Her user name at her calendar server.
const ADA: &str = "ada@example.test";
/// The password her calendar server takes. Made up, as every sign-in here.
const PASSWORD: &str = "made-up-calendar-password-4471";
/// A password her calendar server does not take.
const MISTYPED: &str = "made-up-not-her-calendar-password";
/// The server she rents her calendar from, as she types it.
const SERVER: &str = "dav.example.org";
/// The account it is kept as.
const ACCOUNT: &str = "caldav:ada@example.test@dav.example.org";

/// Ada's workspace, her device and the calendar server that stands in for the one she names:
/// every server it is asked about holds her Family and Work calendars, and takes [`PASSWORD`]
/// alone.
struct Hers {
    workspaces: Workspaces,
    service: ServiceWorkspaceActions,
    workspace: String,
    workspace_id: WorkspaceId,
    ada: DeviceCert,
    owner: DeviceCert,
    /// The servers asked for their calendars, each with the user name asked as.
    asked: Arc<Mutex<Vec<(String, String)>>>,
    _host: TestHost,
    _dir: tempfile::TempDir,
}

impl Hers {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let host = test_host(dir.path(), &key, &owner).await;
        let workspaces = host.workspaces().clone();
        let asked: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
        let told = Arc::clone(&asked);
        workspaces.discover_calendars_through(move |server, username, password| {
            told.lock()
                .unwrap()
                .push((server.to_owned(), username.to_owned()));
            if password != PASSWORD {
                return Err("server rejected the saved credential".to_owned());
            }
            Ok(["family", "work"]
                .into_iter()
                .map(|slug| CalendarCollection {
                    href: format!("{server}dav/{slug}/"),
                    name: format!("{}{}", slug[..1].to_uppercase(), &slug[1..]),
                    read_only: false,
                })
                .collect())
        });
        let (workspace, workspace_id, service) = hosted(&workspaces).await;
        let ada = admit(&workspaces, &workspace, &owner, 1).await;
        Self {
            workspaces,
            service,
            workspace,
            workspace_id,
            ada,
            owner,
            asked,
            _host: host,
            _dir: dir,
        }
    }

    /// `argument` of the connect, asked by `who` on the device `device` certifies: what it
    /// answered, or where it waits for its person, what they are shown.
    async fn connects(
        &self,
        (device, who): (&DeviceCert, &Caller),
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        ask_at(
            &self.service,
            self.workspace_id,
            (device, who),
            workspace_accounts::target(workspace_accounts::ADD_CALENDAR),
            argument,
        )
        .await
    }

    /// Her own connect of `argument`, from her own device.
    async fn herself(&self, argument: Value) -> Result<Value, ActionCallError> {
        self.connects((&self.ada, &caller(&self.ada)), argument)
            .await
    }

    /// The calendar accounts the workspace keeps, as the calendar bridge reads them.
    async fn kept(&self) -> Vec<accounts::Account> {
        self.workspaces
            .workspace_accounts(&self.workspace)
            .await
            .unwrap()
            .of_type(accounts::calendar::TYPE_ID)
    }

    /// The logins the workspace's vault keeps, as her own device lists them.
    async fn vault(&self) -> Vec<Value> {
        let listed = ask_at(
            &self.service,
            self.workspace_id,
            (&self.ada, &caller(&self.ada)),
            person_vault::target(person_vault::LIST),
            json!({}),
        )
        .await
        .expect("her vault answers her own device");
        listed["items"].as_array().expect("its items").clone()
    }

    /// The servers asked for their calendars since the last time this was read.
    fn servers_asked(&self) -> Vec<(String, String)> {
        std::mem::take(&mut *self.asked.lock().unwrap())
    }
}

/// What she types to connect her calendar: where, as whom, with which password.
fn telling(server: &str, password: &str) -> Value {
    json!({"server": server, "username": ADA, "password": password, "name": "Mine"})
}

/// The words `answer` refuses in.
fn refused(what: &str, answer: Result<Value, ActionCallError>) -> String {
    match answer {
        Err(ActionCallError::Refused { reason }) => reason,
        other => panic!("{what} is refused: {other:?}"),
    }
}

/// Whether `account` is connected for reading only, and every calendar of it is one that is not
/// written.
fn kept_for_reading(account: &accounts::Account) -> bool {
    account.config["read_only"] == true
        && account.config["calendars"]
            .as_array()
            .is_some_and(|all| !all.is_empty() && all.iter().all(|one| one["read_only"] == true))
}

/// A server she names by its bare name is that server over https: it is asked for its calendars
/// with what she typed, and the account is kept with every calendar it holds on show and its
/// sign-in in the workspace's vault, never in an answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_server_named_by_its_bare_name_is_connected_over_https() {
    let hers = Hers::new().await;
    let connected = hers
        .herself(telling(SERVER, PASSWORD))
        .await
        .expect("her calendar server takes the sign-in");
    assert_eq!(
        hers.servers_asked(),
        [("https://dav.example.org/".to_owned(), ADA.to_owned())],
        "a bare name is the server over https"
    );
    assert_eq!(connected["id"], ACCOUNT, "{connected}");
    assert_eq!(
        connected["server"], "https://dav.example.org/",
        "{connected}"
    );
    assert_eq!(connected["services"]["calendar"], true, "{connected}");
    assert!(
        !connected.to_string().contains(PASSWORD),
        "never the sign-in"
    );

    let kept = hers.kept().await;
    assert_eq!(kept.len(), 1, "{kept:?}");
    let account = &kept[0];
    assert_eq!(account.id.as_str(), ACCOUNT);
    assert_eq!(account.display_name, "Mine");
    assert_eq!(account.config["server_url"], "https://dav.example.org/");
    assert_eq!(account.config["username"], ADA);
    let calendars: Vec<(&str, bool, bool)> = account.config["calendars"]
        .as_array()
        .expect("its calendars")
        .iter()
        .map(|calendar| {
            (
                calendar["name"].as_str().unwrap(),
                calendar["visible"] == true,
                calendar["read_only"] == true,
            )
        })
        .collect();
    assert_eq!(
        calendars,
        [("Family", true, false), ("Work", true, false)],
        "every calendar it holds shows, and is written"
    );
    assert!(account.config.get("read_only").is_none());
    assert!(!account.config.to_string().contains(PASSWORD));
    assert_eq!(
        hers.vault().await.len(),
        1,
        "its sign-in is a login of the vault"
    );
}

/// An address that would send the sign-in in the clear, that carries a sign-in of its own, or
/// that names nobody is refused in words before any server is asked; a user name of nothing is
/// refused too. A server that refuses the sign-in is said so in its words, and nothing is kept:
/// no account, no login.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_address_or_a_sign_in_a_server_cannot_take_keeps_nothing() {
    let hers = Hers::new().await;
    for (server, words) in [
        ("http://dav.example.org/", "https"),
        ("https://ada:made-up@dav.example.org/", "sign-in"),
        ("  ", "server address"),
    ] {
        let why = refused(server, hers.herself(telling(server, PASSWORD)).await);
        assert!(why.contains(words), "{server}: {why}");
    }
    let nameless = json!({"server": SERVER, "username": "  ", "password": PASSWORD});
    let why = refused("no user name", hers.herself(nameless).await);
    assert!(why.contains("user name"), "{why}");
    assert!(
        hers.servers_asked().is_empty(),
        "no server is asked anything"
    );

    let why = refused(
        "a password it does not take",
        hers.herself(telling(SERVER, MISTYPED)).await,
    );
    assert!(
        why.contains("rejected"),
        "in her calendar server's words: {why}"
    );
    assert!(hers.kept().await.is_empty(), "no account is kept");
    assert!(hers.vault().await.is_empty(), "and no login");
}

/// An account connected for reading only is kept so, every calendar of it as one that is not
/// written, where the calendar bridge reads it. Added again, by her without saying so or by her
/// agent once she allowed it, it is the same one account with the same one login, and still
/// connected for reading only: only removing it ends that.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_calendar_account_connected_for_reading_only_stays_so_whoever_adds_it_again() {
    let hers = Hers::new().await;
    let mut reading_only = telling(SERVER, PASSWORD);
    reading_only["readOnly"] = json!(true);
    let first = hers
        .herself(reading_only)
        .await
        .expect("her calendar server takes the sign-in");
    let kept = hers.kept().await;
    assert!(kept_for_reading(&kept[0]), "{:?}", kept[0].config);

    let again = hers
        .herself(telling(SERVER, PASSWORD))
        .await
        .expect("she adds it again");
    assert_eq!(again["id"], first["id"], "it is the same account");
    let kept = hers.kept().await;
    assert_eq!(kept.len(), 1);
    assert!(
        kept_for_reading(&kept[0]),
        "it stays connected for reading only until she removes it: {:?}",
        kept[0].config
    );
    assert_eq!(hers.vault().await.len(), 1, "one login, signed in again");

    // Removed, it is gone; connected again, it is written.
    ask_at(
        &hers.service,
        hers.workspace_id,
        (&hers.ada, &caller(&hers.ada)),
        workspace_accounts::target(workspace_accounts::REMOVE),
        json!({"account": ACCOUNT}),
    )
    .await
    .expect("she removes it");
    assert!(hers.kept().await.is_empty());
    hers.herself(telling(SERVER, PASSWORD))
        .await
        .expect("she connects it again");
    let kept = hers.kept().await;
    assert!(
        kept[0].config.get("read_only").is_none(),
        "{:?}",
        kept[0].config
    );
}

/// Her agent's connect waits for her, and nothing she is shown holds the password. Asked again
/// before she answered, it waits as it was. Her no connects nothing and asks no server
/// anything; her yes connects it. Asked once more, once it is connected, it adds nothing and
/// nobody is asked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn her_agents_connect_waits_for_her_and_her_no_connects_nothing() {
    let hers = Hers::new().await;
    let page = hers.service.device_page(hers.workspace_id, &hers.ada);
    let agent = caller(&hers.ada).delegated_to(tilo_actions::remote::device_agent_id(
        &hers.ada.user_public_key,
        "scout",
    ));
    let on_her_device = (&hers.ada, &agent);

    let facts = asks(
        hers.connects(on_her_device, telling(SERVER, PASSWORD))
            .await,
    );
    assert!(
        !serde_json::to_string(&facts).unwrap().contains(PASSWORD),
        "she is shown no password: {facts:?}"
    );
    let again = asks(
        hers.connects(on_her_device, telling(SERVER, PASSWORD))
            .await,
    );
    assert_eq!(again.facts, facts.facts, "it waits again, as it was");
    assert!(
        hers.servers_asked().is_empty(),
        "no server is asked before her answer"
    );
    assert!(hers.kept().await.is_empty(), "and nothing is kept");

    // Her no.
    let declined = super::vault_tests::request_of(
        workspace_accounts::target(workspace_accounts::ADD_CALENDAR),
        telling(SERVER, PASSWORD),
    );
    asks(
        super::vault_tests::asked(&hers.service, hers.workspace_id, on_her_device, &declined).await,
    );
    let answer = answered_by(
        &page,
        &caller(&hers.ada),
        (&agent, &declined),
        Offer::Decline,
    )
    .await;
    assert!(answer.is_err(), "declined, it connects nothing: {answer:?}");
    assert!(hers.kept().await.is_empty(), "nothing is connected");
    assert!(
        hers.servers_asked().is_empty(),
        "no server was asked anything"
    );

    // Her yes.
    let allowed = super::vault_tests::request_of(
        workspace_accounts::target(workspace_accounts::ADD_CALENDAR),
        telling(SERVER, PASSWORD),
    );
    asks(
        super::vault_tests::asked(&hers.service, hers.workspace_id, on_her_device, &allowed).await,
    );
    let connected = answered_by(
        &page,
        &caller(&hers.ada),
        (&agent, &allowed),
        Offer::Approve,
    )
    .await
    .expect("allowed, her calendar server takes the sign-in");
    assert_eq!(connected["id"], ACCOUNT, "{connected}");
    assert_eq!(hers.servers_asked().len(), 1);

    // Once more, connected already.
    let why = refused(
        "her agent's connect of what is connected",
        hers.connects(on_her_device, telling(SERVER, PASSWORD))
            .await,
    );
    assert!(why.contains("connected already"), "{why}");
    assert!(hers.servers_asked().is_empty(), "nobody is asked");
    assert_eq!(hers.kept().await.len(), 1);
    assert_eq!(hers.vault().await.len(), 1);
}

/// Another member's agent connects a calendar for the whole workspace once its own person
/// allowed it: Ada reads the account Bo's agent connected. An app acting for nobody, and an app
/// acting for her, connect nothing and ask no server.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn another_members_agent_connects_for_the_workspace_and_an_app_connects_nothing() {
    let hers = Hers::new().await;
    let bo = admit(&hers.workspaces, &hers.workspace, &hers.owner, 2).await;
    let bos_agent = caller(&bo).delegated_to(tilo_actions::remote::device_agent_id(
        &bo.user_public_key,
        "scout",
    ));
    let connecting = super::vault_tests::request_of(
        workspace_accounts::target(workspace_accounts::ADD_CALENDAR),
        telling(SERVER, PASSWORD),
    );
    asks(
        super::vault_tests::asked(
            &hers.service,
            hers.workspace_id,
            (&bo, &bos_agent),
            &connecting,
        )
        .await,
    );
    answered_by(
        &hers.service.device_page(hers.workspace_id, &bo),
        &caller(&bo),
        (&bos_agent, &connecting),
        Offer::Approve,
    )
    .await
    .expect("Bo allowed his agent's connect");
    let kept = hers.kept().await;
    assert_eq!(kept.len(), 1, "the workspace keeps it");
    assert_eq!(kept[0].id.as_str(), ACCOUNT);
    let _ = hers.servers_asked();

    let hers_hex = hex::encode(hers.ada.user_public_key);
    for (who, app) in [
        (
            "an app acting for nobody",
            Caller::local_app("so.tilo.calendar"),
        ),
        (
            "an app acting for her",
            Caller::local_app("so.tilo.calendar").acting_for(hers_hex),
        ),
    ] {
        let mut other = telling("dav.elsewhere.example", PASSWORD);
        other["username"] = json!("cy@example.test");
        let answer = hers.connects((&hers.ada, &app), other).await;
        assert!(answer.is_err(), "{who}: {answer:?}");
    }
    assert!(
        hers.servers_asked().is_empty(),
        "no server is asked for an app"
    );
    assert_eq!(hers.kept().await.len(), 1, "nothing more is kept");
}
