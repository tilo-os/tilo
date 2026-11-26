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

use crate::workspace_accounts::{
    self, MailAccount, MailAccounts, MailSignIns, Provider, SetFilesMode, SyncControl,
};
use collab_types::WorkspaceId;
use serde_json::{Value, json};
use tilo_account_probe::mail_servers::{Found, MailServers, Server};
use tilo_actions::remote::person_vault;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, InvokeArgument, InvokeRequest, ReceiptStatus,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId};
use tilo_user_identity::DeviceCert;

use super::super::tests::{admit, caller, hosted, settled};
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::TestHost as TestHolder;
use crate::workspaces::person_settings::cloud_control::{Mount, Request};
use crate::workspaces::tests::{hosting_device, test_host as test_holder};

/// The domain whose settings document the discovery fake answers for, as a
/// provider that publishes them.
const EXAMPLE: &str = "ada@example.test";

/// What one test's holder and page are.
struct Setup {
    workspaces: Workspaces,
    holder: super::super::ServiceWorkspaceActions,
    device: DeviceCert,
    /// The workspace's owner: the person whose role reaches its vault, which signing an
    /// account in again reads.
    owner: DeviceCert,
    workspace: WorkspaceId,
    _holder: TestHolder,
    _dir: tempfile::TempDir,
}

impl Setup {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let (key, owner) = hosting_device();
        let server = test_holder(dir.path(), &key, &owner).await;
        let workspaces = server.workspaces().clone();
        // The stand-in calendar server is this machine: the holder may call
        // its own network.
        workspaces.call_private_model_addresses(true);
        workspaces.sign_in_accounts_through(|_address, _password| Ok("signed in".into()));
        let (workspace, workspace_id, holder) = hosted(&workspaces).await;
        let device = admit(&workspaces, &workspace, &owner, 1).await;
        Self {
            workspaces,
            holder,
            device,
            owner,
            workspace: workspace_id,
            _holder: server,
            _dir: dir,
        }
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
        self.ask_as(&self.device, resource, action, argument).await
    }

    /// What `action` of `resource` answers, as the page of `device` asks it.
    async fn ask_as(
        &self,
        device: &DeviceCert,
        resource: &str,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        self.ask_target(
            device,
            workspace_accounts::PROVIDER,
            resource,
            action,
            argument,
        )
        .await
    }

    /// What `action` of `resource` of the provider `provider` answers, as the page of
    /// `device` asks it.
    async fn ask_target(
        &self,
        device: &DeviceCert,
        provider: &str,
        resource: &str,
        action: &str,
        argument: Value,
    ) -> Result<Value, ActionCallError> {
        let target = RemoteActionTarget {
            provider: provider.to_owned(),
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
        let page = self.holder.device_page(self.workspace, device);
        let receipt = settled(page.invoke(&caller(device), request)).await?;
        match receipt.status {
            ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or(Value::Null)),
            other => panic!("the holder answers at once, not {other:?}"),
        }
    }

    async fn add_mail(&self, argument: Value) -> Result<MailAccount, ActionCallError> {
        self.ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            argument,
        )
        .await
        .map(|answer| serde_json::from_value(answer).expect("an account reads"))
    }

    async fn listed(&self) -> MailAccounts {
        self.ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::LIST,
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
                workspace_accounts::RESOURCE,
                workspace_accounts::SET_SERVICES,
                json!({"account": EXAMPLE, "mail": false, "calendar": false, "files": false}),
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
            workspace_accounts::RESOURCE,
            workspace_accounts::FIND_SERVERS,
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
        .keep_workspace_account(
            (&keeping, &setup.workspaces.acting_or_own(None), None),
            &older,
        )
        .await
        .unwrap();
    let mut nextcloud = accounts::Account::new("nextcloud:ada@cloud.example.test", "nextcloud");
    nextcloud.display_name = "Cloud".into();
    nextcloud.config = json!({
        "provider": "nextcloud",
        "server_url": "https://cloud.example.test",
        "username": "ada",
        "use_mail": false,
        "use_calendar": true,
        "use_files": false,
    });
    setup
        .workspaces
        .keep_workspace_account(
            (&keeping, &setup.workspaces.acting_or_own(None), None),
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

#[tokio::test]
async fn the_accounts_reading_lists_an_account_connected_signed_in_again_and_removed() {
    let setup = Setup::new().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    setup.discover(seen);

    // The reading is what this device publishes, and it starts empty.
    let record = crate::workspaces::account_logins::accounts_document();
    let mut writes = setup.workspaces.document_writes();
    assert_eq!(reading(&setup).await["accounts"], json!([]));

    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({"address": EXAMPLE, "password": "typed-password"}),
        )
        .await
        .expect("its servers took the sign-in");

    // The accounts record was written, which is what the reading's followers
    // are told about.
    let wrote = loop {
        let write = tokio::time::timeout(std::time::Duration::from_secs(5), writes.recv())
            .await
            .expect("a write reached the followers")
            .expect("the followers are still listening");
        if write.record == record {
            break write;
        }
    };
    assert_eq!(wrote.workspace, setup.workspace.to_string());

    let listed = reading(&setup).await;
    let accounts = listed["accounts"]
        .as_array()
        .expect("the reading is a list");
    assert_eq!(accounts.len(), 1, "{listed}");
    assert_eq!(accounts[0]["id"], json!(EXAMPLE));
    assert_eq!(accounts[0]["kind"], json!("mail"));
    assert_eq!(accounts[0]["name"], json!(EXAMPLE));
    assert_eq!(accounts[0]["address"], json!(EXAMPLE));
    assert_eq!(accounts[0]["status"], json!("never"));
    assert_eq!(accounts[0]["provider"], json!("other"));
    assert_eq!(accounts[0]["enabled"], json!(true));
    assert_eq!(
        accounts[0]["services"],
        json!({"mail": true, "calendar": false, "files": false}),
        "its row says which of its services it feeds"
    );

    // The server stops taking the stored sign-in: signing in again reports
    // that, with the probe's own words, and the reading says so.
    let rejects = Arc::new(std::sync::atomic::AtomicBool::new(true));
    setup.workspaces.sign_in_accounts_through({
        let rejects = Arc::clone(&rejects);
        move |_address, _password| {
            if rejects.load(std::sync::atomic::Ordering::Relaxed) {
                Err("IMAP server rejected the saved credential".into())
            } else {
                Ok("signed in".into())
            }
        }
    });
    let refused = setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::RECONNECT,
            json!({"account": EXAMPLE}),
        )
        .await
        .expect_err("the server no longer takes the sign-in");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason.contains("rejected")),
        "{refused:?}"
    );
    let listed = reading(&setup).await;
    assert_eq!(listed["accounts"][0]["status"], json!("needs-reconnect"));
    assert_eq!(
        listed["accounts"][0]["words"],
        json!("IMAP server rejected the saved credential")
    );

    // The server takes it again: signing in again proves it, and the reading
    // follows.
    rejects.store(false, std::sync::atomic::Ordering::Relaxed);
    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::RECONNECT,
            json!({"account": EXAMPLE}),
        )
        .await
        .expect("the vault's sign-in works again");
    let listed = reading(&setup).await;
    assert_eq!(listed["accounts"][0]["status"], json!("connected"));

    // Removing it lists nothing.
    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::REMOVE,
            json!({"account": EXAMPLE}),
        )
        .await
        .expect("the account goes");
    assert_eq!(reading(&setup).await["accounts"], json!([]));
}

/// An account added in Settings is kept with the servers its person wrote once the server took
/// its sign-in, and its listing says what it feeds; removing it in Settings takes it from Mail.
/// The stand-in server takes one made-up login and speaks IMAP and SMTP over real loopback
/// sockets.
#[tokio::test]
async fn an_account_added_in_settings_is_kept_for_mail_and_removed_with_it() {
    use tilo_account_probe::stand_in;

    const ADDRESS: &str = "team@example.test";
    const PASSWORD: &str = "made-up team password";

    let started = Arc::new(
        tokio::task::spawn_blocking(|| {
            stand_in::started(stand_in::Serving {
                names: vec!["localhost".into()],
                login: ADDRESS.into(),
                password: PASSWORD.into(),
                oauth_token: None,
                imap: "127.0.0.1:0".into(),
                smtp: "127.0.0.1:0".into(),
                imap_wrapped: true,
                smtp_wrapped: false,
                layout: stand_in::Layout::Plain,
                seeded: true,
            })
            .expect("the stand-in mail server listens")
        })
        .await
        .unwrap(),
    );
    let setup = Setup::new().await;
    setup
        .workspaces
        .trust_mail_servers(vec![started.certificate.clone()]);

    // Settings adds the account with the servers its person wrote, and the
    // server takes the sign-in.
    setup
        .add_mail(json!({
            "address": ADDRESS,
            "password": PASSWORD,
            "incomingHost": format!("localhost:{}", started.imap.port()),
            "outgoingHost": format!("localhost:{}", started.smtp.port()),
        }))
        .await
        .expect("its server took the sign-in");

    // The account is kept, without its password, with the servers its sign-in belongs to: what
    // the mail connector signs in to.
    let listed = setup.listed().await;
    assert_eq!(listed.accounts.len(), 1, "{listed:?}");
    assert_eq!(listed.accounts[0].id, ADDRESS);
    assert!(listed.accounts[0].has_secret);
    assert!(
        !serde_json::to_string(&listed).unwrap().contains(PASSWORD),
        "a listing carries no password"
    );

    // Removing it in Settings takes it from Mail: its listing no longer has it.
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::REMOVE,
            json!({"account": ADDRESS}),
        )
        .await
        .expect("the account goes");
    assert!(
        setup.listed().await.accounts.is_empty(),
        "Mail lists no account that Settings removed"
    );
}

/// The reading a workspace's apps follow, as this host keeps it.
async fn reading(setup: &Setup) -> Value {
    setup
        .workspaces
        .accounts_reading(&setup.workspace.to_string())
        .await
}

/// An iCloud account asked for calendars keeps them beside its mail: the calendar account is
/// kept where the workspace's accounts are and signed in with the vault's entry its mail uses,
/// the mail account names it, and turning its calendars off or removing the mail account takes
/// it with them.
#[cfg(feature = "agents")]
#[tokio::test]
#[allow(
    clippy::too_many_lines,
    reason = "one journey: add with calendars, turn them off, on again and remove the account"
)]
async fn an_icloud_account_asked_for_calendars_keeps_them_beside_its_mail() {
    const ADDRESS: &str = "ada@icloud.example";
    const CALENDAR: &str = "https://caldav.example.test/";

    let setup = Setup::new().await;
    setup.workspaces.keep_icloud_calendars_at(CALENDAR);
    setup
        .workspaces
        .find_mail_servers_through(move |asked, allowed| {
            if asked != ADDRESS {
                return Err(format!("No mail server was found for {asked}"));
            }
            let _ = allowed("imap.mail.me.com");
            Ok(MailServers {
                incoming: Server::new("imap.mail.me.com", 993),
                outgoing: Server::new("smtp.mail.me.com", 587),
                found: Found::Published,
                username: Some(ADDRESS.to_owned()),
                own_sign_in: None,
            })
        });

    let kept = setup
        .add_mail(json!({
            "address": ADDRESS,
            "password": "app-password",
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
            .any(|account| account.id == ADDRESS && account.services.calendar),
        "Mail lists it too, feeding calendars: {listed:?}"
    );

    // The account is kept where the workspace's accounts are, and signed in with an entry of
    // the vault: the password its person typed once.
    let workspace = setup.workspace.to_string();
    let held = setup
        .workspaces
        .workspace_accounts(&workspace)
        .await
        .unwrap();
    let config = settings(&setup, ADDRESS).await;
    let linked = config["calendar_account"]
        .as_str()
        .expect("the calendar account is named")
        .to_owned();
    assert_eq!(config["server"], json!(CALENDAR));
    let calendar = held.account(&linked).expect("the calendar account is kept");
    assert_eq!(calendar.type_id.as_str(), accounts::calendar::TYPE_ID);
    assert_eq!(calendar.config["mail_account"], json!(ADDRESS));
    let sign_in = accounts::email::password_item(&calendar.config)
        .expect("the calendar account names its sign-in");
    let sign_in = person_vault::ItemRef::parse(sign_in).expect("a vault entry");
    assert!(
        setup
            .workspaces
            .vault_keeps(&workspace, &sign_in)
            .await
            .unwrap(),
        "the vault keeps the calendar account's sign-in"
    );

    // Turning its calendars off takes the calendar account with it.
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SET_SERVICES,
            json!({"account": ADDRESS, "mail": true, "calendar": false, "files": false}),
        )
        .await
        .expect("its member says what it feeds");
    let config = settings(&setup, ADDRESS).await;
    assert!(config.get("calendar_account").is_none());
    let held = setup
        .workspaces
        .workspace_accounts(&workspace)
        .await
        .unwrap();
    assert!(
        held.account(&linked).is_none(),
        "the calendar account went with its service"
    );
    assert_eq!(
        reading(&setup).await["accounts"]
            .as_array()
            .and_then(|accounts| accounts
                .iter()
                .find(|account| account["id"] == json!(ADDRESS)))
            .map(|account| account["services"].clone()),
        Some(json!({"mail": true, "calendar": false, "files": false})),
        "the row says its calendars are off"
    );

    // Turning them on again keeps them with the sign-in the vault holds.
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SET_SERVICES,
            json!({"account": ADDRESS, "mail": true, "calendar": true, "files": false}),
        )
        .await
        .expect("its member says what it feeds");
    let config = settings(&setup, ADDRESS).await;
    let linked = config["calendar_account"]
        .as_str()
        .expect("the calendar account is named again")
        .to_owned();
    let held = setup
        .workspaces
        .workspace_accounts(&workspace)
        .await
        .unwrap();
    let calendar = held
        .account(&linked)
        .expect("the calendar account is kept again");
    let sign_in =
        accounts::email::password_item(&calendar.config).expect("it names its sign-in again");
    let sign_in = person_vault::ItemRef::parse(sign_in).expect("a vault entry");
    assert!(
        setup
            .workspaces
            .vault_keeps(&workspace, &sign_in)
            .await
            .unwrap(),
        "the sign-in the vault holds keeps it"
    );
    assert_eq!(
        reading(&setup).await["accounts"]
            .as_array()
            .and_then(|accounts| accounts
                .iter()
                .find(|account| account["id"] == json!(ADDRESS)))
            .map(|account| account["services"].clone()),
        Some(json!({"mail": true, "calendar": true, "files": false})),
        "the row says its calendars are on again"
    );

    // Removing the account takes the calendar account with it.
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::REMOVE,
            json!({"account": ADDRESS}),
        )
        .await
        .expect("the account goes");
    let held = setup
        .workspaces
        .workspace_accounts(&workspace)
        .await
        .unwrap();
    assert!(held.account(&linked).is_none(), "the calendar account went");
    assert!(held.account(ADDRESS).is_none(), "the mail account went");
    assert!(
        setup
            .ask(
                workspace_accounts::RESOURCE,
                workspace_accounts::LIST,
                Value::Null,
            )
            .await
            .is_ok(),
        "Mail lists its accounts after the removal"
    );
}

/// The servers a person writes are kept as written, a port of their own decides how the
/// connection starts, an edited account keeps the identities it had, and a written server that
/// does not read is refused before anything is written.
#[tokio::test]
async fn a_mail_account_keeps_the_servers_its_person_wrote() {
    let setup = Setup::new().await;
    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({
                "address": EXAMPLE,
                "password": "typed-password",
                "incomingHost": "imap.example.test:143",
                "outgoingHost": "smtp.example.test:587",
                "identities": ["Ada <ada@example.test>"],
            }),
        )
        .await
        .expect("the written servers took the sign-in");
    let config = settings(&setup, EXAMPLE).await;
    assert_eq!(config["imap_host"], "imap.example.test");
    assert_eq!(config["imap_port"], 143);
    assert_eq!(config["imap_tls"], false, "port 143 upgrades with STARTTLS");
    assert_eq!(config["smtp_host"], "smtp.example.test");
    assert_eq!(config["smtp_port"], 587);
    assert_eq!(config["identities"], json!(["Ada <ada@example.test>"]));

    // A port and name that do not read are refused, and change nothing.
    let wrong = setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({"address": EXAMPLE, "password": "typed-password", "incomingHost": "imaps://x"}),
        )
        .await
        .expect_err("an address is no server");
    assert!(
        matches!(&wrong, ActionCallError::Refused { reason } if reason.contains("incoming server")),
        "{wrong:?}"
    );

    // An edit that types no identities keeps the ones the account had, and the servers read as
    // stored.
    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({
                "address": EXAMPLE,
                "password": "",
                "displayName": "Ada Work",
                "incomingHost": "imap.example.test:143",
                "outgoingHost": "smtp.example.test:587",
            }),
        )
        .await
        .expect("the same servers keep the saved sign-in");
    let config = settings(&setup, EXAMPLE).await;
    assert_eq!(config["display_name"], "Ada Work");
    assert_eq!(config["identities"], json!(["Ada <ada@example.test>"]));
    assert_eq!(
        config["import_pending"],
        json!(false),
        "the saved sign-in keeps the account watching its inbox"
    );

    // The reading writes the servers as its person did, for the edit form.
    let listed = reading(&setup).await;
    assert_eq!(
        listed["accounts"][0]["incoming"],
        json!("imap.example.test:143")
    );
    assert_eq!(
        listed["accounts"][0]["outgoing"],
        json!("smtp.example.test:587")
    );
    assert_eq!(listed["accounts"][0]["username"], json!(EXAMPLE));
}

/// Another member of the workspace lists the account the first one added, and checks its server;
/// a check writes nothing of its own.
#[tokio::test]
async fn another_member_lists_the_account_and_checks_its_server() {
    let setup = Setup::new().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    setup.discover(seen);
    setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({"address": EXAMPLE, "password": "typed-password"}),
        )
        .await
        .expect("the address finds its servers");

    let bo = admit(
        &setup.workspaces,
        &setup.workspace.to_string(),
        &setup.owner,
        2,
    )
    .await;
    let kept = setup
        .workspaces
        .workspace_accounts(&setup.workspace.to_string())
        .await
        .expect("the workspace's accounts read");
    assert_eq!(kept.of_type(accounts::email::TYPE_ID).len(), 1);
    assert!(
        serde_json::to_string(&kept.of_type(accounts::email::TYPE_ID))
            .expect("the record writes")
            .find("typed-password")
            .is_none(),
        "no password is in the record"
    );
    let listed = setup
        .ask_as(
            &bo,
            workspace_accounts::RESOURCE,
            workspace_accounts::LIST,
            Value::Null,
        )
        .await
        .expect("the other member lists the accounts");
    assert_eq!(listed["accounts"].as_array().map(Vec::len), Some(1));

    let checked = setup
        .ask_as(
            &setup.owner,
            workspace_accounts::RESOURCE,
            workspace_accounts::CHECK,
            json!({"account": EXAMPLE}),
        )
        .await
        .expect("the saved sign-in is checked");
    assert_eq!(checked, json!("signed in"));
}

/// A login the workspace's vault keeps for Ada's mail servers adds her account without a
/// password being typed again: the listing offers every login the vault keeps, by its
/// reference, name and places, never its secret, and the account signs in with that entry.
#[tokio::test]
async fn a_login_the_vault_keeps_adds_its_account() {
    let setup = Setup::new().await;
    let (reference, site) = logins_kept_in_the_vault(&setup).await;

    // The accounts listing offers the logins the vault keeps, and says nothing of what they
    // sign in with.
    the_listing_offers_every_kept_login(&setup, &reference, &site).await;

    // Adding from it keeps the account, signing in with that entry and nothing copied.
    an_account_added_from_its_login(&setup, &reference).await;
}

/// Ada's mail login, and a login for a site that is no mail server, kept in the workspace's
/// vault: their references.
async fn logins_kept_in_the_vault(setup: &Setup) -> (String, String) {
    let kept = setup
        .ask_target(
            &setup.owner,
            person_vault::PROVIDER,
            person_vault::RESOURCE,
            person_vault::ADD,
            json!({
                "kind": "login",
                "title": EXAMPLE,
                "belongsTo": ["imap://imap.example.test:993", "smtp://smtp.example.test:465"],
                "username": EXAMPLE,
                "secret": "kept-password",
            }),
        )
        .await
        .expect("the workspace's vault keeps her login");
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    // A login for a site that is no mail server is the vault's too, and is listed as it is.
    let site = setup
        .ask_target(
            &setup.owner,
            person_vault::PROVIDER,
            person_vault::RESOURCE,
            person_vault::ADD,
            json!({
                "kind": "login",
                "title": "example.test",
                "belongsTo": ["https://example.test"],
                "username": "ada",
                "secret": "site-password",
            }),
        )
        .await
        .expect("the workspace's vault keeps her site login");
    let site = site["reference"].as_str().expect("a reference").to_owned();
    (reference, site)
}

/// The accounts listing offers the logins the vault keeps, by reference, name and places, and
/// says nothing of what they sign in with.
async fn the_listing_offers_every_kept_login(setup: &Setup, reference: &str, site: &str) {
    let offered: MailSignIns = serde_json::from_value(
        setup
            .ask(
                workspace_accounts::RESOURCE,
                workspace_accounts::SIGN_INS,
                Value::Null,
            )
            .await
            .expect("the workspace's logins are read"),
    )
    .expect("the login listing reads");
    assert_eq!(offered.sign_ins.len(), 2, "{offered:?}");
    let login = offered
        .sign_ins
        .iter()
        .find(|login| login.id == reference)
        .expect("her mail login is offered");
    assert_eq!(login.id, reference);
    assert_eq!(login.title, EXAMPLE);
    assert_eq!(login.username, EXAMPLE);
    assert_eq!(
        login.places,
        vec![
            "imap://imap.example.test:993".to_owned(),
            "smtp://smtp.example.test:465".to_owned()
        ]
    );
    let site_login = offered
        .sign_ins
        .iter()
        .find(|login| login.id == site)
        .expect("her site login is offered too");
    assert_eq!(site_login.places, vec!["https://example.test".to_owned()]);
    let said = serde_json::to_string(&offered).unwrap();
    assert!(!said.contains("kept-password"), "{said}");
    assert!(!said.contains("site-password"), "{said}");
}

/// Adding an account from the kept login `reference` keeps it, signing in with that entry and
/// nothing copied.
async fn an_account_added_from_its_login(setup: &Setup, reference: &str) {
    let added: MailAccount = serde_json::from_value(
        setup
            .ask(
                workspace_accounts::RESOURCE,
                workspace_accounts::ADD_MAIL_WITH_ITEM,
                json!({
                    "address": EXAMPLE,
                    "incomingHost": "imap.example.test",
                    "incomingPort": 993,
                    "outgoingHost": "smtp.example.test",
                    "outgoingPort": 465,
                    "password": reference,
                }),
            )
            .await
            .expect("the account is added from the login"),
    )
    .unwrap();
    assert_eq!(added.address, EXAMPLE);
    assert!(added.has_secret);
    let config = settings(setup, EXAMPLE).await;
    assert_eq!(
        config[accounts::email::PASSWORD_ITEM_KEY],
        json!(reference),
        "the account names the login it signs in with"
    );
    assert!(
        setup
            .ask(
                workspace_accounts::RESOURCE,
                workspace_accounts::LIST,
                Value::Null,
            )
            .await
            .is_ok(),
        "the listing answers after the account is added"
    );
}

/// What tells this device's cloud mounts what to do, as a test stands in for the files
/// service: every request it was told, and the mounts it answers with.
#[derive(Default)]
struct StandInCloud {
    mounts: Mutex<Vec<Mount>>,
    requests: Mutex<Vec<Request>>,
}

impl crate::workspaces::person_settings::cloud_control::CloudControl for StandInCloud {
    fn mounts(&self) -> Result<Vec<Mount>, String> {
        Ok(self.mounts.lock().unwrap().clone())
    }

    fn control(&self, request: Request) -> Result<(), String> {
        self.requests.lock().unwrap().push(request);
        Ok(())
    }
}

impl StandInCloud {
    /// The next request it is told, or a panic naming what it was told instead.
    fn told(&self, expected: &Request) {
        let requests = self.requests.lock().unwrap();
        assert!(
            requests.last() == Some(expected),
            "the files service was told {requests:?}, not {expected:?}"
        );
    }
}

/// A cloud account's files mode is chosen on it, and its sync is controlled on this device's
/// files service: sync now, pause and resume each reach the service, and the reading says
/// whether the mount is paused.
#[tokio::test]
async fn a_cloud_accounts_files_mode_and_its_sync_are_chosen() {
    let setup = Setup::new().await;
    let cloud = Arc::new(StandInCloud::default());
    setup.workspaces.cloud_control_through(Arc::clone(&cloud)
        as Arc<dyn crate::workspaces::person_settings::cloud_control::CloudControl>);
    let id = "google-drive:my-drive";
    keep_the_drive(&setup, id).await;

    // Its files stay in sync until copied once; the choice is kept on the account.
    its_files_mode_is_chosen(&setup, id).await;

    // Sync now, pause and resume each reach this device's files service.
    sync_controls_reach_the_files_service(&setup, &cloud, id).await;

    // A mail account's files do not sync here, so its sync is refused.
    a_mail_accounts_files_do_not_sync(&setup).await;

    // The reading says a paused mount is paused.
    a_paused_mount_reads_paused(&setup, &cloud, id).await;
}

/// `id`, a drive whose files sync, kept on the workspace as one of this device's accounts.
async fn keep_the_drive(setup: &Setup, id: &str) {
    let mut drive = accounts::Account::new(id, accounts::cloud_types::GOOGLE_DRIVE);
    drive.display_name = "My Drive".into();
    drive.config = json!({
        "provider": "google-drive",
        "use_files": true,
        "use_mail": false,
        "use_calendar": false,
        "services": {"mail": false, "calendar": false, "files": true},
    });
    let keeping = setup.workspace.to_string();
    setup
        .workspaces
        .keep_workspace_account(
            (&keeping, &setup.workspaces.acting_or_own(None), None),
            &drive,
        )
        .await
        .unwrap();
}

/// The drive `id` copies its files once when its person chooses, and no mode there is none of
/// is kept.
async fn its_files_mode_is_chosen(setup: &Setup, id: &str) {
    assert_eq!(
        accounts::cloud_types::mode_of(&settings(setup, id).await),
        Some(accounts::cloud_types::FilesMode::Sync)
    );
    let kept: SetFilesMode = SetFilesMode {
        account: id.to_owned(),
        mode: "import".to_owned(),
    };
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SET_FILES_MODE,
            serde_json::to_value(&kept).unwrap(),
        )
        .await
        .expect("its person chooses to copy it once");
    assert_eq!(
        accounts::cloud_types::mode_of(&settings(setup, id).await),
        Some(accounts::cloud_types::FilesMode::Import)
    );
    let refused = setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SET_FILES_MODE,
            json!({"account": id, "mode": "sometimes"}),
        )
        .await
        .expect_err("a mode there is none of is refused");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason.contains("files mode")),
        "{refused:?}"
    );
}

/// Sync now, pause and resume each reach this device's files service.
async fn sync_controls_reach_the_files_service(setup: &Setup, cloud: &StandInCloud, id: &str) {
    let sync = serde_json::to_value(SyncControl {
        account: id.to_owned(),
    })
    .unwrap();
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SYNC_NOW,
            sync.clone(),
        )
        .await
        .expect("the service is told to sync now");
    cloud.told(&Request::SyncNow {
        account: id.to_owned(),
    });
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::PAUSE_SYNC,
            sync.clone(),
        )
        .await
        .expect("the service is told to pause");
    cloud.told(&Request::Pause {
        account: id.to_owned(),
    });
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::RESUME_SYNC,
            sync,
        )
        .await
        .expect("the service is told to resume");
    cloud.told(&Request::Resume {
        account: id.to_owned(),
    });
}

/// A mail account's files do not sync on a device, so its sync is refused.
async fn a_mail_accounts_files_do_not_sync(setup: &Setup) {
    setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::ADD_MAIL,
            json!({"address": EXAMPLE, "password": "typed", "incomingHost": "imap.example.test", "outgoingHost": "smtp.example.test"}),
        )
        .await
        .expect("a mail account is added");
    let refused = setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::SYNC_NOW,
            json!({"account": EXAMPLE}),
        )
        .await
        .expect_err("a mail account's files do not sync on a device");
    assert!(
        matches!(&refused, ActionCallError::Refused { reason } if reason.contains("cloud-storage")),
        "{refused:?}"
    );
}

/// The reading says a paused mount is paused, and its files mode is the one chosen.
async fn a_paused_mount_reads_paused(setup: &Setup, cloud: &StandInCloud, id: &str) {
    *cloud.mounts.lock().unwrap() = vec![Mount {
        account: id.to_owned(),
        paused: true,
    }];
    let reading = reading(setup).await;
    let account = reading["accounts"]
        .as_array()
        .and_then(|accounts| accounts.iter().find(|account| account["id"] == json!(id)))
        .expect("the drive reads");
    assert_eq!(account["paused"], json!(true));
    assert_eq!(account["filesMode"], json!("import"));
}

/// A request of the accounts' action `action`, as the daemon's own connection makes one.
fn cloud_request(action: &str, argument: Value) -> InvokeRequest {
    InvokeRequest {
        invocation: InvocationId::new(),
        target: workspace_accounts::target(action),
        argument: match argument {
            Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
            _ => InvokeArgument::None,
        },
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A cloud mount on this computer is handed its account's sign-in by the workspace that keeps
/// it, as the person's own connection, and no page of theirs is: the secret never crosses an
/// app's or a page's connection.
#[tokio::test]
async fn a_cloud_mount_is_handed_its_sign_in_by_the_workspace() {
    use tilo_actions::remote::Caller;
    use zeroize::Zeroizing;

    let setup = Setup::new().await;
    let workspace = setup.workspace.to_string();
    let person = hex::encode(setup.owner.user_public_key);
    // The sign-in is the workspace host's own write for the person whose mount reads it.
    let kept_by = caller(&setup.owner);
    let place = accounts::vault_items::Place::parse("https://dav.example.test").unwrap();
    let reference = setup
        .workspaces
        .keep_account_password(
            &workspace,
            (&kept_by, None),
            None,
            crate::workspaces::account_logins::AccountLogin {
                title: "Work files".into(),
                username: "ada".into(),
                places: vec![place],
            },
            Zeroizing::new("dav-password".to_owned()),
        )
        .await
        .expect("the workspace's vault keeps the account's password");
    let mut account = accounts::Account::new("files-1", "webdav");
    account.display_name = "Work files".into();
    account.config = json!({"server_url": "https://dav.example.test", "username": "ada"});
    accounts::email::set_password_item(&mut account.config, Some(&reference.to_string()));
    setup
        .workspaces
        .keep_workspace_account((&workspace, kept_by.acting(), None), &account)
        .await
        .expect("the workspace keeps the account");

    let own = Caller::own_person(person).in_workspace(workspace.clone());
    let listed = setup
        .workspaces
        .person_accounts_invoke(
            Some(setup.workspace),
            &own,
            None,
            &cloud_request(workspace_accounts::CLOUD_ACCOUNTS, Value::Null),
        )
        .await
        .expect("the mount reads the accounts it keeps");
    assert_eq!(listed["accounts"][0]["account"]["id"], json!("files-1"));
    assert_eq!(listed["accounts"][0]["account"]["type_id"], json!("webdav"));

    let credential = setup
        .workspaces
        .person_accounts_invoke(
            Some(setup.workspace),
            &own,
            None,
            &cloud_request(
                workspace_accounts::CLOUD_CREDENTIAL,
                json!({"account": "files-1"}),
            ),
        )
        .await
        .expect("the mount is handed the account's password");
    assert_eq!(credential["kind"], json!("basic"));
    assert_eq!(credential["username"], json!("ada"));
    assert_eq!(credential["secret"], json!("dav-password"));

    // A page of the member's is not a connection of their own: the same ask there is refused.
    let refused = setup
        .ask(
            workspace_accounts::RESOURCE,
            workspace_accounts::CLOUD_CREDENTIAL,
            json!({"account": "files-1"}),
        )
        .await;
    assert!(
        matches!(refused, Err(ActionCallError::Refused { .. })),
        "a page is handed no sign-in: {refused:?}"
    );

    setup
        .workspaces
        .person_accounts_invoke(
            Some(setup.workspace),
            &own,
            None,
            &cloud_request(
                workspace_accounts::REPORT_SYNC,
                json!({"account": "files-1", "health": "healthy"}),
            ),
        )
        .await
        .expect("the mount reports its sync");
    let held = setup
        .workspaces
        .workspace_accounts(&workspace)
        .await
        .unwrap();
    assert_eq!(
        held.sync_of("files-1").map(|sync| sync.health),
        Some(accounts::SyncHealth::Healthy)
    );
}
