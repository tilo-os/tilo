//! The mail another mail program kept on this computer, which the OS installer brought over as
//! an imported account's mail: Mail takes it in before the account's server is asked, so a
//! person sees the mail they had even where the server is gone.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "a failed unwrap or panic in a test is its assertion"
)]

use mail_engine::model::AccountConfig;
use tilolang_mail::connector::{Event, MailConnector as _};
use tilolang_mail::engine::{AccountSource, EngineConnector, MailAccount};

/// What the OS installer writes for an account whose mail it brought over: the engine's store's
/// own shape, one file an account, messages in the folders they were in.
const KEPT: &str = r#"{
  "version": 1,
  "messages": [
    {
      "id": "6f2b1a3c-0000-4000-8000-000000000001",
      "rfc_message_id": "plan@hopper.example",
      "in_reply_to": null,
      "from": { "name": "Grace Hopper", "email": "grace@hopper.example" },
      "to": [{ "name": null, "email": "sam@rivera.example" }],
      "cc": [],
      "subject": "Kept plan",
      "body": "Here is the plan, as it was kept.",
      "attachments": [],
      "timestamp_min": 29500000,
      "read": true,
      "starred": false,
      "folder": "inbox",
      "labels": [],
      "account": "sam@rivera.example",
      "server": null
    },
    {
      "id": "6f2b1a3c-0000-4000-8000-000000000002",
      "rfc_message_id": "old@hopper.example",
      "in_reply_to": null,
      "from": { "name": "Grace Hopper", "email": "grace@hopper.example" },
      "to": [{ "name": null, "email": "sam@rivera.example" }],
      "cc": [],
      "subject": "Filed away",
      "body": "An archived one.",
      "attachments": [],
      "timestamp_min": 29400000,
      "read": true,
      "starred": false,
      "folder": "archive",
      "labels": [],
      "account": "sam@rivera.example",
      "server": null
    }
  ]
}"#;

fn connector(config: AccountConfig, root: std::path::PathBuf) -> EngineConnector {
    let (requests, _answered) = std::sync::mpsc::channel();
    let source: AccountSource = Box::new(move || vec![MailAccount::from(config.clone())]);
    EngineConnector::sharing(source, requests).keeping(Some(root))
}

fn cache_dir() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("mail-kept-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("a cache directory");
    root
}

/// The kept mail is an imported account's mail before its server is asked: each mailbox answers
/// with its own, once, and the messages keep their folder and read state.
#[test]
fn the_mail_the_installer_kept_is_taken_in_once_in_its_folders() {
    let root = cache_dir();
    std::fs::write(root.join("sam@rivera.example.json"), KEPT).expect("the kept mail");
    let config = AccountConfig {
        email: "sam@rivera.example".into(),
        import_pending: true,
        ..AccountConfig::default()
    };
    let mut mail = connector(config, root.clone());

    assert!(
        mail.changes("sam@rivera.example", "inbox").is_some(),
        "the kept mail answers the fetch"
    );
    let said = mail.take_ready();
    let [Event::Changes(changes)] = said.as_slice() else {
        panic!("one answer of kept mail: {said:?}");
    };
    assert_eq!(changes.messages.len(), 1, "only the inbox's own");
    assert_eq!(changes.messages[0].subject, "Kept plan");
    assert_eq!(changes.messages[0].mailbox, "inbox");
    assert!(!changes.messages[0].unread, "kept mail was read already");
    assert!(
        mail.take_ready().is_empty(),
        "the mailbox's mail is taken in once"
    );

    mail.changes("sam@rivera.example", "archive");
    let said = mail.take_ready();
    let [Event::Changes(changes)] = said.as_slice() else {
        panic!("one answer of kept mail: {said:?}");
    };
    assert_eq!(changes.messages.len(), 1);
    assert_eq!(changes.messages[0].mailbox, "archive");

    std::fs::remove_dir_all(&root).expect("the cache directory goes");
}

/// An account the installer did not import, or one it kept nothing for, is asked of its server:
/// nothing answers for it from the cache.
#[test]
fn an_account_with_nothing_kept_asks_its_server() {
    let root = cache_dir();
    std::fs::write(root.join("sam@rivera.example.json"), KEPT).expect("the kept mail");
    // Signed in to its server: its mail is the server's.
    let signed_in = AccountConfig {
        email: "sam@rivera.example".into(),
        import_pending: false,
        ..AccountConfig::default()
    };
    let mut mail = connector(signed_in, root.clone());
    assert!(
        !matches!(mail.take_ready().as_slice(), [Event::Changes(_)]),
        "nothing is answered from the cache"
    );
    // Imported, but with nothing kept.
    let untouched = AccountConfig {
        email: "nobody@rivera.example".into(),
        import_pending: true,
        ..AccountConfig::default()
    };
    let mut empty = connector(untouched, root.clone());
    assert!(empty.changes("nobody@rivera.example", "inbox").is_none());
    assert!(empty.take_ready().is_empty());
    std::fs::remove_dir_all(&root).expect("the cache directory goes");
}
