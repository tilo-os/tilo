//! A workspace's accounts on the holder that keeps them: the servers of an
//! address are found from the address alone, an account is added from its
//! address and password, and the services an account feeds are turned on and
//! off.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "in a test these are the assertion, not a hazard, and the holder a test binds is kept until it ends"
)]

use std::sync::{Arc, Mutex};

use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_account_probe::mail_servers::{Found, MailServers, Server};
use tilo_actions::remote::person_accounts::{self, MailAccount, MailAccounts, Provider};
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, governed, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost as TestHolder;
use crate::workspaces::tests::{governor, test_host as test_holder};

/// The domain whose settings document the discovery fake answers for, as a
/// provider that publishes them.
const EXAMPLE: &str = "ada@example.test";

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
            signed: None,
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
}

/// What an account's settings keep, as the workspace's accounts hold them.
async fn settings(setup: &Setup, id: &str) -> Value {
    setup
        .workspaces
        .workspace_accounts(&setup.workspace.to_string())
        .await
        .unwrap()
        .account(id)
        .map(|account| account.config)
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

    // What it keeps says what it feeds, for the next reading and for Mail.
    let config = settings(&setup, EXAMPLE).await;
    assert_eq!(
        config["services"],
        json!({"mail": true, "calendar": false, "files": false})
    );

    // Its member turns its mail off: the account stays, feeding nothing.
    let listed: MailAccounts = serde_json::from_value(
        setup
            .ask(
                person_accounts::RESOURCE,
                person_accounts::SET_SERVICES,
                json!({"account": EXAMPLE, "services": {"mail": false}}),
            )
            .await
            .expect("its member says what it feeds"),
    )
    .unwrap();
    assert_eq!(listed.accounts.len(), 1);
    assert!(!listed.accounts[0].services.mail);
    assert_eq!(
        settings(&setup, EXAMPLE).await["services"],
        json!({"mail": false, "calendar": false, "files": false})
    );
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

/// The listing answers every account a workspace keeps: an account added before
/// it said who provides it defaults to the provider its servers name and
/// mail alone, and a Nextcloud server's account is listed with no mail
/// server at all.
#[tokio::test]
async fn the_listing_answers_older_accounts_and_ones_that_feed_no_mail() {
    let setup = Setup::new().await;
    let mut older = accounts::Account::new(EXAMPLE, accounts::email::TYPE_ID);
    older.display_name = "Ada".into();
    older.config = json!({
        "email": EXAMPLE,
        "imap_host": "imap.example.test",
        "imap_port": 993,
        "smtp_host": "smtp.example.test",
        "smtp_port": 465,
    });
    let keeping = setup.workspace.to_string();
    setup
        .workspaces
        .keep_workspace_account((&keeping, &setup.workspaces.acting_or_own(None)), &older)
        .await
        .unwrap();
    let mut nextcloud = accounts::Account::new("nextcloud:ada@cloud.example.test", "nextcloud");
    nextcloud.display_name = "Cloud".into();
    nextcloud.config = json!({
        "provider": "nextcloud",
        "server_url": "https://cloud.example.test",
        "username": "ada",
        "services": {"mail": false, "calendar": true, "files": false},
    });
    setup
        .workspaces
        .keep_workspace_account(
            (&keeping, &setup.workspaces.acting_or_own(None)),
            &nextcloud,
        )
        .await
        .unwrap();

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

/// A provider that keeps calendars with the same app password keeps them
/// beside the mail account when its person asks for calendars: one connect
/// feeds Mail and Calendar alike, and turning calendars off takes the
/// calendar account with it.
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey: add with calendars, turn them off, on again and remove the account"
)]
async fn an_icloud_account_asked_for_calendars_keeps_them_beside_its_mail() {
    use crate::workspaces::person_settings::calendars::{self as held, fake_caldav};

    let setup = Setup::new().await;
    let address = "ada@icloud.example";
    let password = "app-password";
    let calendar = fake_caldav::Server::start(address, password);
    calendar.add_calendar("personal", "Personal");
    setup.workspaces.keep_icloud_calendars_at(&calendar.url());
    setup
        .workspaces
        .find_mail_servers_through(move |asked, allowed| {
            if asked != address {
                return Err(format!("No mail server was found for {asked}"));
            }
            let _ = allowed("imap.mail.me.com");
            Ok(MailServers {
                incoming: Server::new("imap.mail.me.com", 993),
                outgoing: Server::new("smtp.mail.me.com", 587),
                found: Found::Published,
                username: Some(address.to_owned()),
                own_sign_in: None,
            })
        });

    let kept = setup
        .add_mail(json!({
            "address": address,
            "password": password,
            "services": {"mail": true, "calendar": true},
        }))
        .await
        .expect("its servers were found and took the sign-in");
    assert_eq!(kept.provider, Provider::Icloud);
    assert!(kept.services.calendar, "one connect feeds both");
    let listed = setup.listed().await;
    assert!(
        listed
            .accounts
            .iter()
            .any(|account| account.id == address && account.services.calendar),
        "Mail lists it too, feeding calendars: {listed:?}"
    );

    // The mail account names the calendar account kept beside it.
    let config = settings(&setup, address).await;
    let linked = config["calendar_account"]
        .as_str()
        .expect("the calendar account is named")
        .to_owned();
    assert_eq!(config["server"], json!(calendar.url()));
    let kept_calendars = setup
        .ask(held::RESOURCE, held::LIST, Value::Null)
        .await
        .expect("Calendar lists the account's calendars");
    assert!(
        kept_calendars["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts
                .iter()
                .any(|account| account["id"] == json!(linked))),
        "Calendar lists the calendar account: {kept_calendars}"
    );

    // Turning its calendars off takes the calendar account with it.
    setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::SET_SERVICES,
            json!({"account": address, "services": {"mail": true, "calendar": false}}),
        )
        .await
        .expect("its member says what it feeds");
    let config = settings(&setup, address).await;
    assert!(config.get("calendar_account").is_none());
    let kept_calendars = setup
        .ask(held::RESOURCE, held::LIST, Value::Null)
        .await
        .expect("Calendar lists no account now");
    assert!(
        !kept_calendars["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts
                .iter()
                .any(|account| account["id"] == json!(linked))),
        "the calendar account went with its service: {kept_calendars}"
    );

    // Turning them on again keeps them with the sign-in the vault holds, and
    // removing the account takes them with it.
    setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::SET_SERVICES,
            json!({"account": address, "services": {"mail": true, "calendar": true}}),
        )
        .await
        .expect("its member says what it feeds");
    let kept_calendars = setup
        .ask(held::RESOURCE, held::LIST, Value::Null)
        .await
        .expect("Calendar lists the calendars again");
    assert!(
        kept_calendars["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts
                .iter()
                .any(|account| account["id"] == json!(linked))),
        "Calendar lists the calendar account again: {kept_calendars}"
    );
    setup
        .ask(
            person_accounts::RESOURCE,
            person_accounts::REMOVE,
            json!({"account": address}),
        )
        .await
        .expect("the account goes");
    let kept_calendars = setup
        .ask(held::RESOURCE, held::LIST, Value::Null)
        .await
        .expect("Calendar lists nothing now");
    assert!(
        !kept_calendars["accounts"]
            .as_array()
            .is_some_and(|accounts| accounts
                .iter()
                .any(|account| account["id"] == json!(linked))),
        "the calendar account went with the account: {kept_calendars}"
    );
    assert!(
        setup
            .ask(
                person_accounts::RESOURCE,
                person_accounts::LIST,
                Value::Null,
            )
            .await
            .is_ok(),
        "Mail lists its accounts after the removal"
    );
}
