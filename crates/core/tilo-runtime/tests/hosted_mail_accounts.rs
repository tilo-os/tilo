//! A mail account moved in from Thunderbird is connected, not copied: `tilo migrate start
//! thunderbird` hands the account's servers, the other address it sends as and the password
//! Thunderbird saved to the workspace as the person's own request
//! (`workspace/mail-accounts#add` over `/v1`), and the host signs in before it keeps anything.
//! Mail then reads the account's inbox from its mail server through the host, signed in with the
//! password the vault keeps; nothing is delivered from a cache.
//!
//! The person's laptop reads the inbox over the device link, as a page of theirs does. The mail
//! server itself is not stood in for here: the fixture's stand-in speaks a session type this
//! crate cannot name, so the read reaches the address the account names and stops there.

use serde_json::{Value, json};
use tilo_actions::InvocationUpdate;
use tilo_actions::remote::person_accounts;

use crate::host;

/// What `tilo migrate` asks, as resource and action (thunderbird.rs `MAIL_ACCOUNTS`).
const ACCOUNTS: &str = "workspace/mail-accounts";
/// The account, made up for this file.
const ADDRESS: &str = "ada@mail.example.test";
/// What Thunderbird saved for it.
const SAVED: &str = "saved-by-thunderbird-for-the-stand-in";

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

/// The account's actions in the workspace, as `/v1` answers them.
async fn act(key: &host::Key, workspace: &str, action: &str, args: &Value) -> (u16, Value) {
    key.post(
        &format!("/v1/workspaces/{workspace}/resources/{ACCOUNTS}/actions/{action}"),
        args,
    )
    .await
}

/// A migrated account is connected and its mail is read from its server: a sign-in the mail
/// server refuses is no account, the saved password signs in and is kept with its servers and
/// its other address, and the inbox reads from the server through the host, not a cache.
#[test]
fn a_migrated_account_is_connected_and_its_mail_is_read_from_its_server() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        let proven = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let proving = std::sync::Arc::clone(&proven);
        workspaces.sign_in_accounts_through(move |account, password| {
            proving.lock().unwrap().push(account.to_owned());
            if password == SAVED {
                Ok("signed in".into())
            } else {
                Err("IMAP server rejected the credential".into())
            }
        });
        // The importer acts as the laptop `tilo login` made of her computer: a key of hers that
        // reaches the workspace.
        let key = host.key("Ada's laptop");

        // A password the mail server does not take is refused in its words, and nothing is kept.
        let (status, refused) = act(
            &key,
            &workspace,
            "add",
            &from_thunderbird("not-what-thunderbird-saved"),
        )
        .await;
        assert!(
            status != 200 || refused["status"] != "ok",
            "a sign-in the mail server refuses is no account: {refused}"
        );
        let said = refused.to_string();
        assert!(said.contains("rejected the credential"), "{said}");
        assert!(!said.contains("not-what-thunderbird-saved"), "{said}");
        let (status, listed) = act(&key, &workspace, "list", &json!({})).await;
        assert_eq!(status, 200, "{listed}");
        assert_eq!(listed["result"]["accounts"], json!([]), "{listed}");

        // The saved password: signed in to, then kept with its servers and its other address.
        let (status, added) = act(&key, &workspace, "add", &from_thunderbird(SAVED)).await;
        assert_eq!(status, 200, "{added}");
        assert_eq!(added["status"], "ok", "{added}");
        assert_eq!(added["result"]["address"], ADDRESS, "{added}");
        assert_eq!(added["result"]["hasSecret"], true, "{added}");
        assert!(
            !added.to_string().contains(SAVED),
            "an answer carries no password"
        );
        assert_eq!(proven.lock().unwrap().len(), 2, "each add signs in first");

        // Mail, in every window of hers, lists it with the address it also sends as.
        let (status, listed) = act(&key, &workspace, "list", &json!({})).await;
        assert_eq!(status, 200, "{listed}");
        let accounts = listed["result"]["accounts"].as_array().unwrap();
        assert_eq!(accounts.len(), 1, "{listed}");
        assert_eq!(accounts[0]["incomingHost"], "imap.mail.example.test");
        assert_eq!(accounts[0]["outgoingHost"], "smtp.mail.example.test");
        assert_eq!(
            accounts[0]["identities"],
            json!(["Ada at the lab <ada.lab@example.test>"])
        );
        assert!(!listed.to_string().contains(SAVED));

        // Mail reads the inbox from the mail server through the host, as a page of hers does:
        // signed in with the password the vault keeps, no copy of a cache in between.
        let laptop = host.member("Ada's laptop", &workspace).await;
        let target = person_accounts::target(person_accounts::FETCH);
        let read = laptop
            .call(
                &target.provider,
                target.address,
                &json!({"account": ADDRESS, "folder": "inbox"}),
            )
            .await
            .unwrap_or_else(|refused| panic!("the inbox is read: {refused:?}"));
        let InvocationUpdate::Succeeded { output, .. } = &read else {
            panic!("the inbox is read: {read:?}");
        };
        let read = output.value.clone().unwrap_or_default();
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
        let _ = proven;
    });
}
