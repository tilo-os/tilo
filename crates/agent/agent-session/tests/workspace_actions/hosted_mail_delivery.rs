//! Mail brought into a hosted workspace through the action API, as `tilo migrate start
//! thunderbird -w` brings each message: `mail/mail.messages#deliver-message` over
//! `POST /v1/workspaces/{workspace}/resources/…/actions/…`, by the same client and call the
//! importer makes. The messages are kept in parts beside the person's mailbox, so the delivery
//! is addressed to them, and the mailbox then lists what arrived.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::*;
use serde_json::{Value, json};

/// Where `tilo migrate` delivers a message (thunderbird.rs `DELIVER`), as resource and action.
const MESSAGES: &str = "mail/mail.messages";
const DELIVER: &str = "deliver-message";

/// The subjects a mailbox search lists, newest first.
fn subjects(listing: &Value) -> Vec<String> {
    listing["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("a search of the mailbox lists messages: {listing}"))
        .iter()
        .map(|message| message["subject"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_delivers_mail_into_a_hosted_workspace_as_tilo_migrate_does() {
    let on_server = api_on_server(ISSUER).await;
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    approved_key(&on_server, &ada, &page, "Ada's import").await;
    // The importer acts as a device of hers, as `tilo login` keeps one on her computer.
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("laptop.json");
    std::fs::write(
        &key_file,
        serde_json::to_vec(&tilo_api::direct::KeyFile::new(
            &on_server.server,
            Some(&on_server.web_entry),
            "Ada's laptop",
            &laptop.key,
            &laptop.cert,
        ))
        .unwrap(),
    )
    .unwrap();
    let client = tilo_api::client::Client::open(tilo_api::client::Reach {
        api: None,
        key_file: Some(&key_file),
        json: false,
    })
    .unwrap();
    let made = client
        .post("/v1/workspaces", &json!({"name": "Ada's mail"}))
        .await
        .unwrap();
    let workspace = made["id"].as_str().unwrap().to_owned();

    // Each message as the importer sends it: in its folder, read as the account had it, under
    // the import's id and a step id of its own.
    let messages = [
        json!({
            "from": "Dana Ruiz <dana@example.test>",
            "to": "ada@example.test",
            "subject": "Offsite venue",
            "body": "Please book the harbour venue for 14 people.",
            "date": "2026-10-12T09:00:00Z",
            "folder": "inbox",
            "read": false,
            "conversation": "venue@example.test",
        }),
        json!({
            "from": "Ada <ada@example.test>",
            "to": "dana@example.test",
            "subject": "Re: Offsite venue",
            "body": "Booked.",
            "date": "2026-10-12T10:00:00Z",
            "folder": "sent",
            "read": true,
            "conversation": "venue@example.test",
        }),
    ];
    for (step, message) in messages.iter().enumerate() {
        let answer = client
            .act_invoked(
                &workspace,
                MESSAGES,
                DELIVER,
                message,
                "migrate-thunderbird",
                "Bring Thunderbird in",
                &format!("migrate-thunderbird-{step}"),
            )
            .await
            .unwrap_or_else(|error| panic!("{MESSAGES}#{DELIVER}: {error:#}"));
        assert_eq!(answer["status"], "ok", "{answer}");
        assert!(
            answer["result"]["value"]["id"].is_string() || answer["result"]["id"].is_string(),
            "a delivery answers the message it kept: {answer}"
        );
    }

    // Mail lists them where they were: the request in the inbox, the reply in Sent.
    for (folder, subject) in [("inbox", "Offsite venue"), ("sent", "Re: Offsite venue")] {
        let found = client
            .act(
                &workspace,
                "mail/mail.mailbox",
                "search-messages",
                &json!({"query": "venue", "folder": folder}),
                "look for the mail",
            )
            .await
            .unwrap();
        assert_eq!(found["status"], "ok", "{found}");
        let listing = found["result"]
            .get("value")
            .cloned()
            .unwrap_or_else(|| found["result"].clone());
        assert_eq!(subjects(&listing), [subject], "{folder}: {listing}");
    }
}
