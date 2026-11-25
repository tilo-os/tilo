//! A person's mail kept by Mail's own connector: the account Settings keeps in the workspace's
//! accounts is the one the connector signs in with, and its server's mail shows in the app and
//! its messages leave through the server.
//!
//! The connector is the app's own, run by the daemon beside its instances: its protocol code is
//! the connector crate's, over the same mail server a person rents.

use serde_json::json;
use tilolang_mail::test_support::{Rig, served};

const ADA: &str = "ada@example.test";
const PASSWORD: &str = "the-password-her-vault-keeps";

/// A workspace with a mail account kept in its accounts record, as Settings keeps one when its
/// person adds it: the server and the sign-in name its sign-in is released for, and never the
/// password.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_connected_mail_account_is_kept_where_the_bridge_reads_it() {
    let device = super::Device::start("Mail", Some(accounts::person::ACCOUNTS_OWNER)).await;
    let workspace = device.workspace.clone();
    let acting = tilo_action_policy::Acting::person(device.person());

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
    device
        .workspaces()
        .keep_workspace_account((&workspace, &acting, None), &account)
        .await
        .expect("the account is kept");

    // What the connector's account source reads: the workspace's email accounts, each with the
    // servers and user name its sign-in is released for.
    let held = device
        .workspaces()
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
    assert!(
        !serde_json::to_string(&kept).unwrap().contains(PASSWORD),
        "a kept account carries no password"
    );

    device.shutdown().await;
}

/// The app shows what its server holds — a conversation with the message on it — and a message
/// its person writes leaves through the server's SMTP endpoint.
#[test]
fn the_app_shows_its_servers_mail_and_sends_one() {
    let server = served(ADA, PASSWORD);
    let mut rig = Rig::new(&server, ADA, PASSWORD);
    rig.settle_until(|rig| rig.accounts().len() == 1);

    // The person sees her account, standing well, and the mail her server holds.
    let account = rig.accounts().remove(0);
    rig.settle_until(|rig| !rig.threads(&account).is_empty());
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
    rig.settle_until(|_| !server.sent().is_empty());
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
    rig.settle_until(|rig| rig.accounts().len() == 1);

    let account = rig.accounts().remove(0);
    rig.settle_until(|rig| !rig.text("Account", &account, "trouble").is_empty());
    assert!(
        !rig.text("Account", &account, "trouble").is_empty(),
        "the refusal is said in the account's words"
    );
    assert!(
        rig.threads(&account).is_empty(),
        "nothing of the server's mail is kept"
    );
}
