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

/// Ada's import key on a fresh server, the client `tilo migrate` opens with it, and a new
/// workspace of hers.
async fn importer() -> (
    super::api_on_server::OnServer,
    tilo_api::client::Client,
    String,
    tempfile::TempDir,
) {
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
    (on_server, client, workspace, keys)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_import_delivers_mail_into_a_hosted_workspace_as_tilo_migrate_does() {
    let (_on_server, client, workspace, _keys) = importer().await;

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

/// The `tilo.content:` reference `bytes` are kept under in `workspace`, uploaded as the
/// importer uploads each (`Hosted::put`).
async fn uploaded(client: &tilo_api::client::Client, workspace: &str, bytes: &[u8]) -> String {
    let answer = client.upload(workspace, bytes.to_vec()).await.unwrap();
    answer["content"].as_str().unwrap().to_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_message_of_40_mib_comes_into_a_hosted_workspace_whole_with_its_parts_apart() {
    use tilo_mail_parts::fixture::message_with_attachments;
    let (_on_server, client, workspace, _keys) = importer().await;
    let files = [
        ("survey.pdf", 24 * 1024 * 1024),
        ("plan.png", 6 * 1024 * 1024),
    ];
    let sent = message_with_attachments("survey@example.test", "Site survey", &files);
    assert!(sent.len() > 40 * 1024 * 1024, "{} bytes", sent.len());

    // As `tilo migrate` delivers it: the message kept and each large part's pieces, uploaded.
    let split = tilo_mail_parts::split(&sent);
    let original = uploaded(&client, &workspace, &split.rest).await;
    let mut parts = Vec::new();
    for part in &split.apart {
        let mut pieces = Vec::new();
        for piece in part.pieces() {
            pieces.push(uploaded(&client, &workspace, piece).await);
        }
        parts.push(json!({"at": part.at, "size": part.size, "pieces": pieces}));
    }
    let answer = client
        .act_invoked(
            &workspace,
            MESSAGES,
            DELIVER,
            &json!({
                "from": "Grace Hopper <grace@example.net>",
                "to": "Ada <ada@example.com>",
                "subject": "Site survey",
                "body": "The files are attached.",
                "date": "2026-11-03T10:00:00Z",
                "folder": "inbox",
                "message_id": "<survey@example.test>",
                "original": original,
                "parts": parts,
            }),
            "migrate-thunderbird",
            "Bring Thunderbird in",
            "migrate-thunderbird-large",
        )
        .await
        .unwrap_or_else(|error| panic!("{MESSAGES}#{DELIVER}: {error:#}"));
    assert_eq!(answer["status"], "ok", "{answer}");
    let value = answer["result"]
        .get("value")
        .cloned()
        .unwrap_or_else(|| answer["result"].clone());
    let id = value["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{answer}"))
        .to_owned();

    // Mail reads it whole: its words, and both attachments by name and size.
    let read = client
        .act(
            &workspace,
            MESSAGES,
            "read",
            &json!({"item": id}),
            "read the survey",
        )
        .await
        .unwrap_or_else(|error| panic!("read: {error:#}"));
    assert_eq!(read["status"], "ok", "{read}");
    let read = read["result"]
        .get("value")
        .cloned()
        .unwrap_or_else(|| read["result"].clone());
    assert_eq!(read["message"]["kept_whole"], true, "{read}");
    let listed: Vec<(String, u64)> = read["message"]["attachments"]
        .as_array()
        .unwrap_or_else(|| panic!("{read}"))
        .iter()
        .map(|file| {
            (
                file["name"].as_str().unwrap().to_owned(),
                file["size"].as_u64().unwrap(),
            )
        })
        .collect();
    let expected: Vec<(String, u64)> = files
        .iter()
        .map(|(name, size)| ((*name).to_owned(), u64::try_from(*size).unwrap()))
        .collect();
    assert_eq!(listed, expected);

    // Exported, it is the message that was sent, whole.
    let exported = client
        .act(
            &workspace,
            "mail/mail.mailbox",
            "export-content",
            &json!({"format": "eml", "message": id}),
            "export the survey",
        )
        .await
        .unwrap();
    assert_eq!(exported["status"], "ok", "{exported}");
    let exported = exported["result"]
        .get("value")
        .cloned()
        .unwrap_or_else(|| exported["result"].clone());
    assert_eq!(
        exported["bytes"].as_u64(),
        u64::try_from(sent.len()).ok(),
        "{exported}"
    );
}

/// The message `sent` delivered as `tilo migrate` delivers it: the message kept and each large
/// part's pieces, uploaded. Its id in the mailbox.
async fn deliver(client: &tilo_api::client::Client, workspace: &str, sent: &[u8]) -> String {
    let split = tilo_mail_parts::split(sent);
    let original = uploaded(client, workspace, &split.rest).await;
    let mut parts = Vec::new();
    for part in &split.apart {
        let mut pieces = Vec::new();
        for piece in part.pieces() {
            pieces.push(uploaded(client, workspace, piece).await);
        }
        parts.push(json!({"at": part.at, "size": part.size, "pieces": pieces}));
    }
    let answer = client
        .act_invoked(
            workspace,
            MESSAGES,
            DELIVER,
            &json!({
                "from": "Grace Hopper <grace@example.net>",
                "to": "Ada <ada@example.com>",
                "subject": "Scans",
                "body": "The scans are attached.",
                "date": "2026-11-03T10:00:00Z",
                "folder": "inbox",
                "message_id": "<scans@example.test>",
                "original": original,
                "parts": parts,
            }),
            "migrate-thunderbird",
            "Bring Thunderbird in",
            "migrate-thunderbird-scans",
        )
        .await
        .unwrap_or_else(|error| panic!("{MESSAGES}#{DELIVER}: {error:#}"));
    assert_eq!(answer["status"], "ok", "{answer}");
    value(&answer)["id"]
        .as_str()
        .unwrap_or_else(|| panic!("{answer}"))
        .to_owned()
}

/// What a settled action answered.
fn value(answer: &Value) -> Value {
    answer["result"]
        .get("value")
        .cloned()
        .unwrap_or_else(|| answer["result"].clone())
}

/// The bytes behind the `tilo.content:` reference `content`, downloaded through the API as
/// `tilo` downloads a file.
async fn download(client: &tilo_api::client::Client, workspace: &str, content: &Value) -> Vec<u8> {
    let content = content
        .as_str()
        .unwrap_or_else(|| panic!("no content: {content}"));
    let mut bytes = Vec::new();
    client
        .download(workspace, content, &mut bytes, &mut |_, _| {})
        .await
        .unwrap_or_else(|error| panic!("download {content}: {error:#}"));
    bytes
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_attachment_and_an_export_larger_than_a_part_download_whole() {
    use tilo_mail_parts::fixture::{attachment_bytes, message_with_attachments};
    let (_on_server, client, workspace, _keys) = importer().await;
    let size = 30 * 1024 * 1024;
    let sent = message_with_attachments("scans@example.test", "Scans", &[("scans.pdf", size)]);
    let id = deliver(&client, &workspace, &sent).await;

    // Handed over as content larger than one part, it downloads as the file.
    let handed = client
        .act(
            &workspace,
            &format!("mail/mail.attachment:{id}:0"),
            "content",
            &json!({"message": id, "index": 0}),
            "take the scans",
        )
        .await
        .unwrap_or_else(|error| panic!("content: {error:#}"));
    assert_eq!(handed["status"], "ok", "{handed}");
    let handed = value(&handed);
    assert_eq!(handed["name"], "scans.pdf", "{handed}");
    assert_eq!(handed["size"].as_u64(), Some(size as u64), "{handed}");
    let file = download(&client, &workspace, &handed["content"]).await;
    assert!(
        file == attachment_bytes("scans.pdf", size),
        "{} of {size} bytes",
        file.len()
    );

    // Exported, it downloads as the message that was sent, byte for byte.
    let exported = client
        .act(
            &workspace,
            "mail/mail.mailbox",
            "export-content",
            &json!({"format": "eml", "message": id}),
            "export the scans",
        )
        .await
        .unwrap();
    assert_eq!(exported["status"], "ok", "{exported}");
    let exported = value(&exported);
    assert_eq!(exported["bytes"].as_u64(), Some(sent.len() as u64));
    let eml = download(&client, &workspace, &exported["content"]).await;
    assert!(eml == sent, "{} of {} bytes", eml.len(), sent.len());
}

/// How long downloading a file of `size` bytes put into `workspace` takes, after checking it
/// comes back whole.
async fn timed_download(
    client: &tilo_api::client::Client,
    workspace: &str,
    dir: &std::path::Path,
    size: usize,
) -> std::time::Duration {
    let bytes: Vec<u8> = (0..size)
        .map(|n: usize| u8::try_from(n % 251).unwrap())
        .collect();
    let path = dir.join(format!("{size}.bin"));
    std::fs::write(&path, &bytes).unwrap();
    let answer = client
        .upload_file(workspace, &path, &mut |_, _| {})
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let out = download(client, workspace, &answer["content"]).await;
    let took = started.elapsed();
    assert!(out == bytes, "{} of {} bytes", out.len(), bytes.len());
    took
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_file_over_100_mib_downloads_in_time_flat_in_its_size() {
    let (_on_server, client, workspace, keys) = importer().await;
    let small = 8 * 1024 * 1024;
    let large = 110 * 1024 * 1024;
    let small_took = timed_download(&client, &workspace, keys.path(), small).await;
    let large_took = timed_download(&client, &workspace, keys.path(), large).await;
    // A byte of the large file costs about what a byte of the small one does: a piece of a
    // download is read where it is, never by reading the file from its start.
    let per_byte = |took: std::time::Duration, size: usize| took.as_secs_f64() / size as f64;
    assert!(
        per_byte(large_took, large) < 4.0 * per_byte(small_took, small),
        "{large} bytes took {large_took:?}, {small} bytes {small_took:?}"
    );
}
