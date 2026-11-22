//! A mail account moved in from Thunderbird is connected, not copied: `tilo migrate start
//! thunderbird` hands the account's servers, the other address it sends as and the password
//! Thunderbird saved to the person's server as their own request (`workspace/mail-accounts#add`
//! over `POST /v1/workspaces/{workspace}/resources/…/actions/…`, by the same client and call the
//! importer makes, as the laptop `tilo login` made), and the server signs in before it keeps
//! anything. Mail then reads the account's inbox from its mail server through her server, as a page
//! does, signed in with the password the vault keeps; nothing is delivered from the cache.
//!
//! The mail server is stood in for: it takes one password, and its inbox holds three messages.

use super::api::ISSUER;
use super::api_on_server::{api_on_server, approved_key};
use super::hosted_secret_use::files_holding;
use super::*;
use serde_json::{Value, json};
use std::sync::Mutex;
use tilo_actions::remote::person_accounts::{
    self, FetchMail, FetchedMail, FetchedMessage, FoundMail, MarkMail, ReadMail, SearchMail,
    SendMail, SentMail,
};
use tilo_server::workspaces::fixtures::mail::{
    Attachment, InboxHeard, InboxSeen, Leaving, MailSession, Watching, held_message,
};

/// What `tilo migrate` asks, as resource and action (thunderbird.rs `MAIL_ACCOUNTS`).
const ACCOUNTS: &str = "workspace/mail-accounts";
/// The account, made up for this file.
const ADDRESS: &str = "ada@mail.example.test";
/// What Thunderbird saved for it: the sign-in of nothing but the stand-in below.
const SAVED: &str = "saved-by-thunderbird-for-the-stand-in";

/// The mail server's stand-in: its inbox holds three messages, and it keeps the password each
/// request signed in with.
#[derive(Default)]
struct MailServer {
    signed_in: Mutex<Vec<String>>,
}

impl MailServer {
    fn took(&self, secret: &accounts::Secret) -> Result<(), String> {
        self.signed_in.lock().unwrap().push(secret.password.clone());
        if secret.password == SAVED {
            Ok(())
        } else {
            Err("IMAP server rejected the credential".into())
        }
    }
}

const UNASKED: &str = "this test asks the mail server for its inbox alone";

impl MailSession for MailServer {
    fn fetch(
        &self,
        _: &accounts::Account,
        secret: &accounts::Secret,
        asking: &FetchMail,
    ) -> Result<FetchedMail, String> {
        self.took(secret)?;
        let messages: Vec<FetchedMessage> = (1..=3)
            .map(|uid| {
                held_message(
                    uid,
                    ("Grace", "grace@example.test"),
                    &format!("On the server {uid}"),
                    "Kept by the mail server, never copied from a cache.",
                )
            })
            .collect();
        Ok(FetchedMail {
            account: asking.account.clone(),
            folder: asking.folder.clone(),
            uid_validity: 1,
            newest_uid: 3,
            messages,
            ..FetchedMail::default()
        })
    }

    fn read(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: &ReadMail,
    ) -> Result<FetchedMessage, String> {
        Err(UNASKED.into())
    }

    fn search(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: &SearchMail,
    ) -> Result<FoundMail, String> {
        Err(UNASKED.into())
    }

    fn save_draft(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: &SendMail,
        _: &[Attachment],
        _: Option<&str>,
    ) -> Result<(), String> {
        Err(UNASKED.into())
    }

    fn send(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: &Leaving,
        _: &[Attachment],
        _: Option<&str>,
    ) -> Result<SentMail, String> {
        Err(UNASKED.into())
    }

    fn mark(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: &MarkMail,
    ) -> Result<(), String> {
        Err(UNASKED.into())
    }

    fn watch(
        &self,
        _: &accounts::Account,
        _: &accounts::Secret,
        _: Option<InboxSeen>,
        _: Arc<dyn InboxHeard>,
    ) -> Result<Watching, String> {
        Err(UNASKED.into())
    }
}

/// The account as `tilo migrate` asks for it from the Thunderbird profile it read.
fn from_thunderbird(password: &str) -> Value {
    json!({
        "address": ADDRESS,
        "displayName": "Ada Lovelace",
        "incomingHost": "imap.mail.example.test",
        "incomingPort": 993,
        "outgoingHost": "smtp.mail.example.test",
        "outgoingPort": 465,
        "identities": ["Ada at the lab <ada.lab@example.test>"],
        "password": password,
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_migrated_account_is_connected_and_its_mail_is_read_from_its_server() {
    let on_server = api_on_server(ISSUER).await;
    let mail = Arc::new(MailServer::default());
    let workspaces = on_server.hosted.server.workspaces().unwrap().clone();
    workspaces.carry_mail_through(mail.clone());
    let proved = Arc::new(Mutex::new(Vec::new()));
    let proving = Arc::clone(&proved);
    workspaces.sign_in_accounts_through(move |account, password| {
        proving.lock().unwrap().push(account.to_owned());
        if password == SAVED {
            Ok("signed in".into())
        } else {
            Err("IMAP server rejected the credential".into())
        }
    });
    let (ada, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let page = enrol(&ada, "Ada's browser", DeviceCapabilities::member());
    approved_key(&on_server, &ada, &page, "Ada's key").await;
    // The importer acts as the laptop `tilo login` made of her computer.
    let laptop = enrol(&ada, "Ada's laptop", DeviceCapabilities::member());
    let keys = tempfile::tempdir().unwrap();
    let key_file = keys.path().join("laptop.json");
    std::fs::write(
        &key_file,
        serde_json::to_vec(
            &tilo_api::direct::KeyFile::new(
                &on_server.server,
                Some(&on_server.web_entry),
                "Ada's laptop",
                &laptop.key,
                &laptop.cert,
            )
            .reaching_at(on_server.hosted.web.host_info().clone()),
        )
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

    // A password the mail server does not take is refused in its words, and nothing is kept.
    let refused = client
        .act(
            &workspace,
            ACCOUNTS,
            "add",
            &from_thunderbird("not-what-thunderbird-saved"),
            "Connect",
        )
        .await
        .expect_err("a sign-in the mail server refuses is no account");
    let said = format!("{refused:#}");
    assert!(said.contains("rejected the credential"), "{said}");
    assert!(!said.contains("not-what-thunderbird-saved"), "{said}");
    let listed = client
        .act(&workspace, ACCOUNTS, "list", &json!({}), "Accounts")
        .await
        .unwrap();
    assert_eq!(listed["result"]["accounts"], json!([]), "{listed}");

    // The saved password: signed in to, then kept with its servers and its other address.
    let added = client
        .act(
            &workspace,
            ACCOUNTS,
            "add",
            &from_thunderbird(SAVED),
            "Connect",
        )
        .await
        .unwrap();
    assert_eq!(added["status"], "ok", "{added}");
    assert_eq!(added["result"]["address"], ADDRESS, "{added}");
    assert_eq!(added["result"]["hasSecret"], true, "{added}");
    assert!(
        !added.to_string().contains(SAVED),
        "an answer carries no password"
    );
    assert_eq!(proved.lock().unwrap().len(), 2, "each add signs in first");

    // Mail, in every window of hers, lists it with the address it also sends as.
    let listed = client
        .act(&workspace, ACCOUNTS, "list", &json!({}), "Accounts")
        .await
        .unwrap();
    let accounts = listed["result"]["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1, "{listed}");
    assert_eq!(accounts[0]["incomingHost"], "imap.mail.example.test");
    assert_eq!(accounts[0]["outgoingHost"], "smtp.mail.example.test");
    assert_eq!(
        accounts[0]["identities"],
        json!(["Ada at the lab <ada.lab@example.test>"])
    );
    assert!(!listed.to_string().contains(SAVED));

    // Mail reads the inbox from the mail server through her server, as a page of hers does:
    // signed in with the password the vault keeps, no copy of a cache in between.
    let holder = Arc::new(tilo_api::server::Server::new(
        on_server.hosted.web.host_info().clone(),
    ));
    let paired = tilo_api::grant_key::GrantKey::held(
        laptop.key.clone(),
        laptop.cert.clone(),
        tilo_clock::unix_now_ms().saturating_mul(1_000_000),
    )
    .unwrap();
    let on_laptop = holder.connect(&paired).await.unwrap();
    let laptop_in = holder.enter(&on_laptop, &workspace).await.unwrap();
    let read = on_laptop
        .device
        .ask_host(
            laptop_in,
            person_accounts::target(person_accounts::FETCH),
            json!({"account": ADDRESS, "folder": "inbox"})
                .as_object()
                .cloned()
                .unwrap(),
        )
        .await
        .unwrap_or_else(|refused| panic!("the inbox is read: {refused:?}"));
    let subjects: Vec<&str> = read["messages"]
        .as_array()
        .unwrap_or_else(|| panic!("{read}"))
        .iter()
        .map(|message| message["subject"].as_str().unwrap())
        .collect();
    assert_eq!(
        subjects,
        ["On the server 1", "On the server 2", "On the server 3"]
    );
    assert_eq!(
        mail.signed_in.lock().unwrap().as_slice(),
        [SAVED],
        "the mail server was signed in to with what the vault keeps"
    );

    // The password is in no file of the server in the clear: the vault seals it.
    let holding = files_holding(on_server.state.path(), SAVED.as_bytes());
    assert!(holding.is_empty(), "in the clear: {holding:?}");
}
