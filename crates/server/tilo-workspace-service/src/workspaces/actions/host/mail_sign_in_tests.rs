//! A workspace's accounts on the holder that keeps them: the servers of an
//! address are found from the address alone, an account is added from its
//! address and password, iCloud's calendars are kept with its app password,
//! the services an account feeds are turned on and off, and removing an
//! account takes the calendar kept as its service with it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard, and the holder a test binds is kept until it ends"
)]

use std::path::Path;
use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_account_probe::mail_servers::{Found, MailServers, Server};
use tilo_actions::remote::person_accounts::{
    self, FoundServers, MailAccount, MailAccounts, Provider,
};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost as TestHolder;
use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};
use crate::workspaces::tests::{governor, test_host as test_holder};

/// The domain whose settings document the discovery fake answers for, as a
/// provider that publishes them.
const EXAMPLE: &str = "ada@example.test";
/// A Google address, whose accounts sign in on Google's page.
const GMAIL: &str = "ada@gmail.com";
/// An iCloud address, whose calendars come with its app password.
const ICLOUD: &str = "ada@icloud.com";
/// The app password both iCloud's mail and calendar accounts sign in with.
const APP_PASSWORD: &str = "icloud-app-password";

/// What one test's holder and page are.
struct Setup {
    workspaces: Workspaces,
    holder: super::super::ServiceWorkspaceActions,
    device: DeviceCert,
    workspace: WorkspaceId,
    _holder: TestHolder,
    _dir: tempfile::TempDir,
}

impl Setup {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = governor();
        let server = test_holder(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        // The stand-in calendar server is this machine: the holder may call
        // its own network.
        workspaces.call_private_model_addresses(true);
        workspaces.sign_in_accounts_through(|_address, _password| Ok("signed in".into()));
        let (workspace, workspace_id, holder) = governed(&workspaces).await;
        let device = admit(&workspaces, &workspace, &owner, 1).await;
        Self {
            workspaces,
            holder,
            device,
            workspace: workspace_id,
            _holder: server,
            _dir: dir,
        }
    }

    fn root(&self) -> std::path::PathBuf {
        self.workspaces
            .workspace_root(&self.workspace.to_string())
    }

    fn page(&self) -> impl tilo_actions::remote::ActionProvider + Clone + Send + 'static {
        self.holder.in_workspace_as(self.workspace, &self.device)
    }

    fn herself(&self) -> Caller {
        caller(&self.device)
    }

    /// Find `address`'s servers in a stand-in network, answering what the
    /// scheme of a small provider, of Google and of iCloud say.
    fn discover(&self, seen: Arc<Mutex<Vec<String>>>) {
        self.workspaces
            .find_mail_servers_through(move |address, allowed| {
                seen.lock().unwrap().push(address.to_owned());
                let (incoming, outgoing, own_sign_in) = match address {
                    EXAMPLE => (("imap.example.test", 993), ("smtp.example.test", 465), None),
                    GMAIL => (
                        ("imap.gmail.com", 993),
                        ("smtp.gmail.com", 587),
                        Some("Google"),
                    ),
                    ICLOUD => (("imap.mail.me.com", 993), ("smtp.mail.me.com", 587), None),
                    _ => {
                        return Err(format!("No mail server was found for {address}"));
                    }
                };
                // Only a name that answers in public is asked anything, as the
                // live network asks it.
                let _ = allowed(incoming.0);
                Ok(MailServers {
                    incoming: Server::new(incoming.0, incoming.1),
                    outgoing: Server::new(outgoing.0, outgoing.1),
                    found: Found::Published,
                    username: Some(address.to_owned()),
                    own_sign_in,
                })
            });
    }

    /// What `action` of `resource` answers, as a member's page asks it.
    async fn ask(
        &self,
        resource: &str,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        let target = RemoteActionTarget {
            provider: person_accounts::PROVIDER.to_owned(),
            address: ActionAddress::new(resource, action),
        };
        let argument = match argument {
            Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
            _ => InvokeArgument::None,
        };
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target,
            argument,
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        let receipt = settled(self.page().invoke(&self.herself(), request)).await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("the holder answers at once, not {other:?}"),
        }
    }

    async fn add_mail(&self, argument: Value) -> Result<MailAccount, ActionCallError> {
        self.ask(
            person_accounts::RESOURCE,
            person_accounts::ADD_MAIL,
            argument,
        )
        .await
        .map(|answer| serde_json::from_value(answer).expect("an account reads"))
    }

    async fn listed(&self) -> MailAccounts {
        self.ask(
            person_accounts::RESOURCE,
            person_accounts::LIST,
            Value::Null,
        )
        .await
        .map(|answer| serde_json::from_value(answer).expect("the listing reads"))
        .expect("a member reads the accounts")
    }

    /// The calendar accounts the workspace keeps, as a member's page reads them.
    async fn calendars(&self) -> Vec<Value> {
        let answer = self
            .ask(held::RESOURCE, held::LIST, Value::Null)
            .await
            .expect("a member reads the calendars");
        answer["accounts"].as_array().expect("accounts").clone()
    }
}

/// What an account's settings keep, as the store holds them.
fn settings(root: &Path, id: &str) -> Value {
    accounts::AccountStore::load_in(root)
        .get(&accounts::AccountId::from(id.to_owned()))
        .map(|account| account.config.clone())
        .expect("the account is kept")
}

/// A member types their address and password and nothing else: the servers
/// are worked out from the address, and the account is added once they took
/// the sign-in.
#[tokio::test]
async fn an_account_is_added_from_its_address_and_password_alone() {
    let setup = Setup::new().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    setup.discover(seen.clone());

    let kept = setup
        .add_mail(json!({"address": EXAMPLE, "password": "typed-password"}))
        .await
        .expect("its servers were found and took the sign-in");
    assert_eq!(kept.address, EXAMPLE);
    assert_eq!(kept.incoming_host, "imap.example.test");
    assert_eq!(kept.incoming_port, 993);
    assert_eq!(kept.outgoing_host, "smtp.example.test");
    assert_eq!(kept.outgoing_port, 465);
    assert_eq!(kept.provider, Provider::Other);
    assert!(kept.services.mail);
    assert!(!kept.services.calendar);
    assert!(kept.has_secret);
    assert_eq!(seen.lock().unwrap().clone(), vec![EXAMPLE.to_owned()]);

    // What it keeps says who provides it and what it feeds, for the next
    // reading and for Mail.
    let config = settings(&setup.root(), EXAMPLE);
    assert_eq!(config["provider"], json!("other"));
    assert_eq!(
        config["services"],
        json!({"mail": true, "calendar": false, "files": false})
    );
}

/// Where a Google address's mail lives is answered, and with whom it signs
/// in: its page offers that sign-in rather than a password.
#[tokio::test]
async fn a_google_address_finds_its_servers_and_names_who_signs_in() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let found = setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::FIND_SERVERS,
            json!({"address": GMAIL}),
        )
        .await
        .expect("its servers are found");
    let found: FoundServers = serde_json::from_value(found).unwrap();
    assert_eq!(found.provider, Provider::Google);
    assert_eq!(found.incoming.name, "imap.gmail.com");
    assert_eq!(found.incoming.port, 993);
    assert_eq!(found.outgoing.name, "smtp.gmail.com");
}

/// An address whose servers nobody answers for is refused in words its
/// member reads, naming the domain whose servers were not found.
#[tokio::test]
async fn finding_servers_that_answer_nowhere_names_the_domain() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let refused = setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::FIND_SERVERS,
            json!({"address": "ada@nowhere.example"}),
        )
        .await
        .expect_err("no server answers for it");
    let ActionCallError::Refused { reason } = refused else {
        panic!("a refusal says why in words: {refused:?}");
    };
    assert_eq!(reason, "No mail servers were found for nowhere.example.");
}

/// A Google address is not added with a password: its member is told to sign
/// in on Google's page instead.
#[tokio::test]
async fn a_google_address_says_to_sign_in_with_google() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let refused = setup
        .add_mail(json!({"address": GMAIL, "password": "app-password"}))
        .await
        .expect_err("Google signs its accounts in on its own page");
    let ActionCallError::Refused { reason } = refused else {
        panic!("a refusal says why in words: {refused:?}");
    };
    assert!(reason.contains("Sign in with Google"), "{reason}");
    assert!(setup.listed().await.accounts.is_empty(), "nothing was kept");
}

/// An iCloud account asked for calendars keeps them with the same app
/// password, at iCloud's own calendar server, and both list as one account's
/// services.
#[tokio::test]
async fn an_icloud_account_keeps_its_calendars_with_its_app_password() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let calendar_server = fake_caldav::Server::start(ICLOUD, APP_PASSWORD);
    calendar_server.add_calendar("work", "Work");
    setup
        .workspaces
        .keep_icloud_calendars_at(&calendar_server.url());

    let kept = setup
        .add_mail(json!({
            "address": ICLOUD,
            "password": APP_PASSWORD,
            "services": {"mail": true, "calendar": true},
        }))
        .await
        .expect("its mail and calendars took the sign-in");
    assert_eq!(kept.provider, Provider::Icloud);
    assert!(kept.services.mail);
    assert!(kept.services.calendar, "its calendars are kept");
    assert_eq!(kept.server, calendar_server.url());

    let calendars = setup.calendars().await;
    assert_eq!(calendars.len(), 1, "{calendars:?}");
    assert_eq!(calendars[0]["serverUrl"], json!(calendar_server.url()));
    assert_eq!(calendars[0]["username"], json!(ICLOUD));
    assert_eq!(
        settings(&setup.root(), ICLOUD)["calendar_account"],
        calendars[0]["id"],
        "the mail account keeps the calendar account's id"
    );

    // One listing answers both as the workspace's one account.
    let listed = setup.listed().await;
    assert_eq!(listed.accounts.len(), 1, "{listed:?}");
    assert_eq!(listed.accounts[0].id, ICLOUD);
}

/// A calendar server that does not take an iCloud account's sign-in leaves
/// the mail account, feeding mail alone and saying so.
#[tokio::test]
async fn an_icloud_calendar_server_that_refuses_leaves_mail_alone() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let calendar_server = fake_caldav::Server::start(ICLOUD, "another-password");
    setup
        .workspaces
        .keep_icloud_calendars_at(&calendar_server.url());

    let kept = setup
        .add_mail(json!({
            "address": ICLOUD,
            "password": APP_PASSWORD,
            "services": {"mail": true, "calendar": true},
        }))
        .await
        .expect("its mail took the sign-in, and its calendars did not");

    assert!(kept.services.mail);
    assert!(!kept.services.calendar, "its calendars were not kept");
    assert!(setup.calendars().await.is_empty());
    assert_eq!(
        settings(&setup.root(), ICLOUD)["services"],
        json!({"mail": true, "calendar": false, "files": false})
    );
}

/// Turning an iCloud account's calendars off removes the calendar account
/// and its copy; turning them on again keeps one with the app password its
/// vault holds. Its mail stays either way.
#[tokio::test]
async fn setting_services_keeps_and_removes_an_icloud_calendar() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let calendar_server = fake_caldav::Server::start(ICLOUD, APP_PASSWORD);
    calendar_server.add_calendar("work", "Work");
    setup
        .workspaces
        .keep_icloud_calendars_at(&calendar_server.url());

    setup
        .add_mail(json!({
            "address": ICLOUD,
            "password": APP_PASSWORD,
            "services": {"mail": true, "calendar": true},
        }))
        .await
        .expect("its mail and calendars took the sign-in");
    assert_eq!(setup.calendars().await.len(), 1);

    let off = setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::SET_SERVICES,
            json!({"account": ICLOUD, "services": {"mail": true, "calendar": false}}),
        )
        .await
        .expect("a member says what the workspace uses");
    let off: MailAccounts = serde_json::from_value(off).unwrap();
    assert_eq!(off.accounts.len(), 1, "the mail account stays");
    assert!(off.accounts[0].services.mail);
    assert!(!off.accounts[0].services.calendar);
    assert!(
        setup.calendars().await.is_empty(),
        "the calendar account went"
    );

    let on = setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::SET_SERVICES,
            json!({"account": ICLOUD, "services": {"mail": true, "calendar": true}}),
        )
        .await
        .expect("its calendars are kept again");
    let on: MailAccounts = serde_json::from_value(on).unwrap();
    assert!(on.accounts[0].services.calendar);
    assert_eq!(
        setup.calendars().await.len(),
        1,
        "a calendar account is back"
    );
}

/// Removing a mail account removes the calendar account kept as its service,
/// and what was kept of both.
#[tokio::test]
async fn removing_an_account_takes_its_calendar_with_it() {
    let setup = Setup::new().await;
    setup.discover(Arc::new(Mutex::new(Vec::new())));
    let calendar_server = fake_caldav::Server::start(ICLOUD, APP_PASSWORD);
    setup
        .workspaces
        .keep_icloud_calendars_at(&calendar_server.url());
    setup
        .add_mail(json!({
            "address": ICLOUD,
            "password": APP_PASSWORD,
            "services": {"mail": true, "calendar": true},
        }))
        .await
        .expect("its mail and calendars took the sign-in");

    let left = setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::REMOVE,
            json!({"account": ICLOUD}),
        )
        .await
        .expect("the account is removed");
    let left: MailAccounts = serde_json::from_value(left).unwrap();
    assert!(left.accounts.is_empty());
    assert!(
        setup.calendars().await.is_empty(),
        "its calendar went with it"
    );
}

/// The listing answers every account a workspace keeps: an account added before
/// it said who provides it defaults to the provider its servers name and
/// mail alone, and a Nextcloud server's account is listed with no mail
/// server at all.
#[tokio::test]
async fn the_listing_answers_older_accounts_and_ones_that_feed_no_mail() {
    let setup = Setup::new().await;
    let root = setup.root();
    let mut store = accounts::AccountStore::load_in(&root);
    let mut older = accounts::Account::new(EXAMPLE, accounts::email::TYPE_ID);
    older.display_name = "Ada".into();
    older.config = json!({
        "email": EXAMPLE,
        "imap_host": "imap.example.test",
        "imap_port": 993,
        "smtp_host": "smtp.example.test",
        "smtp_port": 465,
    });
    store.upsert(older);
    let mut nextcloud = accounts::Account::new("nextcloud:ada@cloud.example.test", "nextcloud");
    nextcloud.display_name = "Cloud".into();
    nextcloud.config = json!({
        "provider": "nextcloud",
        "server_url": "https://cloud.example.test",
        "username": "ada",
        "services": {"mail": false, "calendar": true, "files": false},
    });
    store.upsert(nextcloud);
    store.save_in(&root).unwrap();

    let listed = setup.listed().await;
    assert_eq!(listed.accounts.len(), 2, "{listed:?}");
    let older = &listed.accounts[0];
    assert_eq!(older.address, EXAMPLE);
    assert_eq!(older.provider, Provider::Other);
    assert!(older.services.mail);
    assert!(!older.services.calendar);
    assert_eq!(older.incoming_host, "imap.example.test");
    let cloud = listed
        .accounts
        .iter()
        .find(|account| account.provider == Provider::Nextcloud)
        .expect("the Nextcloud account is listed");
    assert!(cloud.incoming_host.is_empty());
    assert!(cloud.outgoing_host.is_empty());
    assert!(!cloud.services.mail);
    assert!(cloud.services.calendar);
    assert_eq!(cloud.server, "https://cloud.example.test");
    assert_eq!(cloud.address, "ada");
}
