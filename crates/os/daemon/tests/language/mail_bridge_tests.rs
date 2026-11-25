//! A person's mail kept by Mail's own connector: the account Settings keeps in the workspace's
//! accounts is the one the connector signs in with, and its server's mail shows in the app and
//! its messages leave through the server.
//!
//! The connector is the app's own, run by the daemon beside its instances: its protocol code is
//! the connector crate's, over the same mail server a person rents.
//!
//! Its sign-in is the workspace's: the connector reads the accounts the workspace keeps and the
//! password each names from the workspace's vault, through the connection to the device's one
//! dispatch it is handed as Mail.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
use tilo_actions::remote::{Caller, person_vault};
use tilo_workspace_service::work::{
    HostConnections, Order, Turn, connection_holder, takes_turn_in,
};
use tilo_workspace_service::workspaces::MemberDevice;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, governor, record_grants, test_host_holding,
};
use tilolang_mail::test_support::{Rig, served};

const ADA: &str = "ada@example.test";
const PASSWORD: &str = "the-password-her-vault-keeps";
/// Mail's app, as its connector runs as it.
const MAIL: &str = "so.tilo.mail";
/// Another app of the device's, which signs in to nothing.
const NOTES: &str = "so.tilo.notes";

/// A workspace with a mail account kept in its accounts record, as Settings keeps one when its
/// person adds it: the server and the sign-in name its sign-in is released for, and never the
/// password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_mail_account_is_kept_where_the_bridge_reads_it() {
    // The workspace's own store, with no window and no app's package: what the account is kept
    // in is the one record, and the connector reads it there.
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let host = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = host.workspaces().clone();
    workspaces.set_member_device(Arc::new(MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    let workspace = workspaces.create("Mail").await.unwrap().workspace;
    let acting = tilo_action_policy::Acting::person(hex::encode(cert.user_public_key));

    let mut account =
        accounts::Account::new(accounts::AccountId::from(ADA), accounts::email::TYPE_ID);
    account.display_name = "Ada".into();
    account.config = json!({
        "display_name": "Ada",
        "email": ADA,
        "username": ADA,
        "imap_host": "imap.example.test",
        "imap_port": 993,
        "smtp_host": "smtp.example.test",
        "smtp_port": 465,
    });
    workspaces
        .keep_workspace_account((&workspace, &acting, None), &account)
        .await
        .expect("the account is kept");

    // What the connector's account source reads: the workspace's email accounts, each with the
    // servers and user name its sign-in is released for.
    let held = workspaces
        .workspace_accounts(&workspace)
        .await
        .expect("the workspace's accounts are read");
    let kept = held
        .of_type(accounts::email::TYPE_ID)
        .into_iter()
        .find(|kept| kept.id == account.id)
        .expect("the mail account is listed");
    assert_eq!(kept.display_name, "Ada");
    assert_eq!(
        kept.config.get("imap_host").and_then(|v| v.as_str()),
        Some("imap.example.test")
    );
    assert_eq!(
        kept.config.get("smtp_host").and_then(|v| v.as_str()),
        Some("smtp.example.test")
    );
    assert!(kept.enabled, "an added account signs in");
    assert!(
        !serde_json::to_string(&kept).unwrap().contains(PASSWORD),
        "a kept account carries no password"
    );

    host.shutdown().await;
}

/// The app shows what its server holds — a conversation with the message on it — and a message
/// its person writes leaves through the server's SMTP endpoint.
#[test]
fn the_app_shows_its_servers_mail_and_sends_one() {
    let server = served(ADA, PASSWORD);
    let mut rig = Rig::new(&server, ADA, PASSWORD);
    rig.settle_until("her account", |rig| rig.accounts().len() == 1);

    // The person sees her account, standing well, and the mail her server holds.
    let account = rig.accounts().remove(0);
    rig.settle_until("the mail her server holds", |rig| {
        !rig.threads(&account).is_empty()
    });
    assert_eq!(rig.text("Account", &account, "key"), ADA);
    assert_eq!(rig.text("Account", &account, "trouble"), "");
    let threads = rig.threads(&account);
    assert!(!threads.is_empty(), "her server's conversations are here");
    let subjects: Vec<String> = threads
        .iter()
        .map(|thread| rig.text("Thread", thread, "subject"))
        .collect();
    assert!(
        subjects.iter().any(|subject| subject.contains("Welcome")),
        "the server's mail shows: {subjects:?}"
    );

    // She writes one to a colleague, and it leaves through the server.
    rig.run(
        "Mail",
        "compose",
        None,
        &[
            ("account", json!(ADA)),
            ("to", json!("bob@example.test")),
            ("subject", json!("Lunch on Friday?")),
            ("body", json!("Are you free at noon?")),
        ],
    );
    let draft = rig.drafts().remove(0);
    rig.run("Draft", "send", Some(&draft), &[]);
    rig.settle_until("the message to leave through the server", |_| {
        !server.sent().is_empty()
    });
    let sent = server.sent();
    assert_eq!(sent.len(), 1, "one message left");
    let said = String::from_utf8_lossy(&sent[0]).to_string();
    assert!(said.contains("bob@example.test"), "{said}");
    assert!(said.contains("Lunch on Friday?"), "{said}");
}

/// A mail server that refuses the password is said so on the account, and nothing of what it
/// holds is kept: the person sees why, and the app shows no message of that server.
#[test]
fn a_mail_server_that_refuses_the_password_keeps_no_message() {
    let server = served(ADA, PASSWORD);
    let mut rig = Rig::new(&server, ADA, "not-what-the-server-knows");
    rig.settle_until("her account", |rig| rig.accounts().len() == 1);

    let account = rig.accounts().remove(0);
    rig.settle_until("the refusal on the account", |rig| {
        !rig.text("Account", &account, "trouble").is_empty()
    });
    assert!(
        !rig.text("Account", &account, "trouble").is_empty(),
        "the refusal is said in the account's words"
    );
    assert!(
        rig.threads(&account).is_empty(),
        "nothing of the server's mail is kept"
    );
}

/// `app`, installed in this test's own package store with the actions it calls, declaring the
/// workspace's account sign-ins where `signs_in`.
fn install_app(app: &str, signs_in: bool) {
    let catalog = package_store::Catalog::from_env().expect("the test's package store");
    let payload = catalog.user().packages().join(app);
    std::fs::create_dir_all(&payload).expect("the app's payload");
    std::fs::write(payload.join("probe"), b"mail").expect("the app's entrypoint");
    let capabilities = if signs_in {
        "[capabilities]\noptional = [\"account-sign-ins\"]\n\n"
    } else {
        ""
    };
    let manifest = format!(
        r#"[package]
id = "{app}"
version = "0.1.0"
kind = "app"

[publisher]
identity = "ed25519:0000000000000000000000000000000000000000000000000000000000000000"
signature = ""

[actions]
invoke = true

{capabilities}[sandbox]
namespaces = []
seccomp = "tilo-default"
landlock = "strict"

[sandbox.net]
mode = "off"

[entrypoint]
bin = "probe"
type = "gui"
"#
    );
    // What the catalogue reads: the manifest parses, names the app, and declares the sign-ins
    // where it should.
    let view = tilo_app_package::view::ManifestView::from_toml_str(&manifest)
        .expect("the app's manifest reads");
    assert_eq!(view.package_id, app);
    assert_eq!(
        view.declares_capability(tilo_app_package::manifest::ACCOUNT_SIGN_INS),
        signs_in
    );
    let manifest_path = payload.join("manifest.toml");
    std::fs::write(&manifest_path, manifest).expect("the app's manifest");
    let row = format!(
        "[[entry]]\nid = {app:?}\npayload_root = {payload:?}\nmanifest_path = {manifest_path:?}\n\n"
    );
    let registry = catalog.user().registry();
    let mut rows = std::fs::read_to_string(&registry).unwrap_or_default();
    rows.push_str(&row);
    std::fs::write(registry, rows).expect("the registry names the app");
}

/// Mail, installed here and holding its long-lived connection: what the device's trigger runner
/// asks about before it takes that connection's turn.
struct MailInstalled;

impl HostConnections for MailInstalled {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        vec![(MAIL.to_owned(), Vec::new())]
    }
    fn installed(&self, app: &str) -> bool {
        app == MAIL
    }
    fn gives(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
    fn on_power(&self) -> bool {
        true
    }
    fn start(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        true
    }
    fn stop(&self, _: &str, _: &str) {}
}

/// The order here: this is the only device that runs Mail's connection, so it waits for no
/// other.
const AT_ONCE: Order = Order {
    server_grace: Duration::ZERO,
    other_grace: Duration::ZERO,
};

/// The password the workspace's test support keeps for an account it adds, released to the app
/// that signs in.
const KEPT: &str = "kept-for-a-test";

/// The grant that lets Mail read the one entry its account names, as "Approve and grant" on its
/// asked release writes it.
fn sign_in_grant(workspace: &str, person: &str, item: &person_vault::ItemRef) -> Grant {
    Grant::new(
        "test:mail-sign-in",
        Grantee::app(MAIL),
        TreePath::workspace(workspace)
            .app(person_vault::PROVIDER)
            .kind(person_vault::ITEM_KIND)
            .resource(item.to_string())
            .action(person_vault::RELEASE),
        KindSet::READ,
        Answer::Allow,
        Grantee::person(person),
    )
}

/// `address`'s mail account, kept with the sign-in its person typed, as Settings keeps one;
/// answers the vault entry the account names.
async fn add_mail_account(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    address: &str,
) -> person_vault::ItemRef {
    workspaces
        .keep_mail_account_as_added(workspace, address)
        .await
        .expect("the account and its sign-in are kept");
    let held = workspaces.workspace_accounts(workspace).await.unwrap();
    let account = held.account(address).expect("the account the person added");
    let named =
        accounts::email::password_item(&account.config).expect("the account names its sign-in");
    person_vault::ItemRef::parse(named).expect("the sign-in is a vault entry")
}

/// This device running Mail's connection, its claim written: what a release is answered to.
async fn mail_holds_its_connection(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
) {
    workspaces.run_connections(Arc::new(MailInstalled));
    takes_turn_in(
        workspaces,
        workspace,
        &Turn::connection(MAIL, Vec::new()),
        AT_ONCE,
    )
    .await
    .expect("this device takes Mail's connection");
    assert!(
        connection_holder(workspaces, workspace, MAIL)
            .await
            .unwrap()
            .is_some(),
        "the claim is written"
    );
}

/// Mail's connector signs in to the account Settings kept, over the connection to the device's
/// dispatch it holds as Mail: the account's servers and sign-in name come from the workspace's
/// accounts, and its password from the vault entry the account names, released to that
/// connection alone. A disabled account signs in to nothing.
#[test]
fn the_mail_connector_signs_in_with_the_password_the_workspace_keeps() {
    crate::host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let cert = workspaces.device_cert().clone();
        let person = hex::encode(cert.user_public_key);
        let by = Caller::authenticated(cert.user_public_key, cert.device_public_key);
        // The launcher names the connector's workspace, which its connection's calls are made in.
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_WORKSPACE_ID", &workspace);
        install_app(MAIL, true);

        // The person added the account on their device: the workspace's vault keeps its
        // password, and the account names that entry.
        let item = add_mail_account(&workspaces, &workspace, ADA).await;
        // Another account, disabled, is no account the connector signs in to.
        let off = "off@example.test";
        add_mail_account(&workspaces, &workspace, off).await;
        let held = workspaces.workspace_accounts(&workspace).await.unwrap();
        let mut disabled = held.account(off).expect("the account the person added");
        disabled.enabled = false;
        workspaces
            .keep_workspace_account((&workspace, &by.acting(), None), &disabled)
            .await
            .expect("the account is disabled");
        record_grants(
            &workspaces,
            &workspace,
            &[sign_in_grant(&workspace, &person, &item)],
        )
        .await;

        mail_holds_its_connection(&workspaces, &workspace).await;

        // What the dispatch answers Mail's own connection: the logins of its workspace, each
        // with the account it signs in for.
        {
            let probe = tilo_actions::remote::signing_in::SigningIn::over(
                host.daemon()
                    .app_connection_for_test(MAIL, &workspace)
                    .expect("the daemon hands Mail's connection"),
            )
            .expect("Mail's connection reads");
            let told = probe.sign_ins().unwrap_or_else(|refused| {
                panic!("the dispatch refused Mail's sign-ins: {refused}")
            });
            assert!(
                told.sign_ins.iter().any(|login| login
                    .account
                    .as_ref()
                    .is_some_and(|account| account.id == ADA && account.enabled)),
                "Mail's account is listed with its sign-in: {told:?}"
            );
        }

        // The connector's own code, on the connection the daemon answers as Mail.
        let link = host
            .daemon()
            .app_connection_for_test(MAIL, &workspace)
            .expect("the daemon hands Mail's connection");
        let source = tilolang_mail::sign_ins::follow(link, || {});
        tilo_test_env::within("the connector signs in", async {
            while source().is_empty() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        let signed = source();
        assert_eq!(signed.len(), 1, "one mail account signs in");
        let signed = &signed[0];
        assert_eq!(signed.config.email, ADA);
        assert_eq!(signed.config.username, ADA);
        assert_eq!(
            (signed.config.imap_host.as_str(), signed.config.imap_port),
            ("imap.stand-in.test", 993)
        );
        assert_eq!(
            (signed.config.smtp_host.as_str(), signed.config.smtp_port),
            ("smtp.stand-in.test", 465)
        );
        assert_eq!(signed.config.password, KEPT, "the vault's password");
        assert_eq!(signed.refused, None);
    });
}

/// An account added after the connector started reaches it, and one whose entry it may not read
/// yet is refused in the dispatch's words and asked for again: once the person's "Approve and
/// grant" lets it read that entry, the account signs in.
#[test]
fn a_mail_account_added_later_signs_in_once_its_entry_is_allowed() {
    crate::host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let workspaces = host.daemon().workspaces().clone();
        let cert = workspaces.device_cert().clone();
        let person = hex::encode(cert.user_public_key);
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_WORKSPACE_ID", &workspace);
        install_app(MAIL, true);
        mail_holds_its_connection(&workspaces, &workspace).await;

        // Mail's own connection answers the accounts action.
        {
            let probe = tilo_actions::remote::signing_in::SigningIn::over(
                host.daemon()
                    .app_connection_for_test(MAIL, &workspace)
                    .expect("the daemon hands Mail's connection"),
            )
            .expect("Mail's connection reads");
            probe.sign_ins().unwrap_or_else(|refused| {
                panic!("the dispatch refused Mail's sign-ins: {refused}")
            });
        }
        let link = host
            .daemon()
            .app_connection_for_test(MAIL, &workspace)
            .expect("the daemon hands Mail's connection");
        let source = tilolang_mail::sign_ins::follow(link, || {});

        // Nothing is kept yet: the connector reads no account. The account is added after it
        // started, and reaches it on a later answer.
        let item = add_mail_account(&workspaces, &workspace, ADA).await;

        // No grant lets Mail read the entry, so the release is refused, in words, and asked for
        // again on the next answer.
        tilo_test_env::within("the refusal is said", async {
            while source()
                .first()
                .is_none_or(|signed| signed.refused.is_none())
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert_eq!(source()[0].config.password, "", "nothing is released yet");
        assert!(
            source()[0]
                .refused
                .as_deref()
                .is_some_and(|said| !said.contains(KEPT)),
            "the refusal is in words"
        );

        // Their "Approve and grant" lets Mail read that one entry: the account is asked for
        // again, and signs in.
        record_grants(
            &workspaces,
            &workspace,
            &[sign_in_grant(&workspace, &person, &item)],
        )
        .await;
        tilo_test_env::within("the granted entry signs in", async {
            while source()
                .first()
                .is_none_or(|signed| signed.refused.is_some() || signed.config.password != KEPT)
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await;
        assert_eq!(source()[0].config.email, ADA);
    });
}

/// An app whose signed manifest does not declare the account sign-ins reads no login: the
/// connection it is handed refuses the accounts action, and says nothing of a secret it never
/// read.
#[test]
fn an_app_that_does_not_sign_in_is_refused_the_logins() {
    crate::host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let mut environment = tilo_test_env::ScopedEnv::new();
        environment.set("TILO_WORKSPACE_ID", &workspace);
        install_app(NOTES, false);
        // A login the vault keeps, whose account names it: an app that signs in would read it.
        let workspaces = host.daemon().workspaces().clone();
        add_mail_account(&workspaces, &workspace, ADA).await;
        let probe = tilo_actions::remote::signing_in::SigningIn::over(
            host.daemon()
                .app_connection_for_test(NOTES, &workspace)
                .expect("the daemon hands Notes' connection"),
        )
        .expect("Notes' connection reads");
        let refused = probe
            .sign_ins()
            .expect_err("an app that does not sign in reads no login");
        assert!(!refused.is_empty(), "the refusal is in words");
        assert!(
            !refused.contains(KEPT),
            "the refusal tells no secret: {refused}"
        );
    });
}
