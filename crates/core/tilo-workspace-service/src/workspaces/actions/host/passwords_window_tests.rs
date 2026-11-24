//! The workspace's vault as Passwords' own window reads and changes it: what the window keeps
//! lands in the vault, the one place a secret is, and only Passwords reads it or changes it there.

use serde_json::{Map, Value, json};
use tilo_actions::remote::person_vault;

use super::super::tests::{caller, governed};
use super::vault_tests::ask;
use crate::workspaces::fixtures::test_host;
use crate::workspaces::passwords_vault::{
    CARRY, FORGET, GENERATE, KEEP, KEEP_SECRET, ONLY_PASSWORDS, PASSWORDS_APP,
};
use crate::workspaces::tests::governor;

const PASSWORD: &str = "correct horse battery staple";
const ENTRY: &str = "0123456789abcdef0123456789abcdef";

fn arguments(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        other => panic!("arguments are an object, not {other}"),
    }
}

/// The entries the window reads, by id.
fn entry<'a>(reading: &'a Value, id: &str) -> Option<&'a Value> {
    reading["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["id"] == id)
}

/// A login is kept fact by fact and secret by secret, and the vault holds it as one entry that
/// every member's own vault actions read back; nothing the window sent is answered back in a
/// refusal, and a carried entry is taken once.
#[tokio::test]
async fn what_passwords_keeps_is_the_vaults_and_another_app_reaches_none_of_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let invoke = |action: &'static str, argued: Value| {
        let workspace = workspace.clone();
        async move {
            workspaces
                .passwords_vault_invoke(&workspace, PASSWORDS_APP, action, arguments(argued))
                .await
        }
    };

    invoke(
        KEEP,
        json!({"id": ENTRY, "kind": "login", "title": "Bank", "username": "ada",
               "origins": ["https://bank.example"]}),
    )
    .await
    .expect("Passwords keeps what is safe to say of a login");
    invoke(
        KEEP_SECRET,
        json!({"id": ENTRY, "part": "password", "secret": PASSWORD}),
    )
    .await
    .expect("Passwords keeps the password the person typed");
    invoke(
        KEEP_SECRET,
        json!({"id": ENTRY, "part": "field:PIN", "secret": "1234"}),
    )
    .await
    .unwrap();

    let reading = workspaces
        .passwords_vault_reading(&workspace, PASSWORDS_APP)
        .await
        .unwrap();
    let bank = entry(&reading, ENTRY).expect("the vault keeps the login");
    assert_eq!(bank["title"], "Bank");
    assert_eq!(bank["username"], "ada");
    assert_eq!(bank["password"], PASSWORD);
    assert_eq!(bank["origins"], json!(["https://bank.example"]));
    assert_eq!(bank["fields"], json!([{"label": "PIN", "value": "1234"}]));
    assert_eq!(bank["stale"], false, "a password kept now is not old");

    // The one vault: the owner's own vault actions read the same entry, secret included.
    let owned = ask(
        &service,
        workspace_id,
        (&owner, &caller(&owner)),
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .expect("the owner reads the workspace's vault");
    let theirs = owned["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|entry| entry["title"] == "Bank")
        .expect("the entry Passwords kept is the vault's");
    assert_eq!(theirs["password"], PASSWORD);

    // A generated password replaces the typed one where the vault is, and nobody sent it.
    invoke(GENERATE, json!({"id": ENTRY, "length": 24}))
        .await
        .unwrap();
    let reading = workspaces
        .passwords_vault_reading(&workspace, PASSWORDS_APP)
        .await
        .unwrap();
    let generated = entry(&reading, ENTRY).unwrap()["password"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(generated.chars().count(), 24);
    assert_ne!(generated, PASSWORD);

    // An older record's plaintext is carried once: carrying it again changes nothing.
    let older = "fedcba9876543210fedcba9876543210";
    invoke(
        CARRY,
        json!({"id": older, "kind": "wifi", "title": "Home", "password": "old wifi key",
               "notes": "router in the hall", "fields": [{"label": "network", "value": "home"}]}),
    )
    .await
    .expect("an older record's secrets come into the vault");
    invoke(
        CARRY,
        json!({"id": older, "kind": "wifi", "title": "Home", "password": "something else"}),
    )
    .await
    .unwrap();
    let reading = workspaces
        .passwords_vault_reading(&workspace, PASSWORDS_APP)
        .await
        .unwrap();
    let home = entry(&reading, older).unwrap();
    assert_eq!(home["kind"], "wifi");
    assert_eq!(
        home["password"], "old wifi key",
        "a carried entry is taken once"
    );
    assert_eq!(home["notes"], "router in the hall");

    // Another app reads none of it and changes none of it, and is told so without its words.
    let browser = tilo_app_id::apps::BROWSER;
    assert_eq!(
        workspaces
            .passwords_vault_reading(&workspace, browser)
            .await
            .unwrap_err(),
        ONLY_PASSWORDS
    );
    let refused = workspaces
        .passwords_vault_invoke(
            &workspace,
            browser,
            KEEP_SECRET,
            arguments(json!({"id": ENTRY, "part": "password", "secret": "stolen"})),
        )
        .await
        .unwrap_err();
    assert_eq!(refused, ONLY_PASSWORDS);
    let refused = workspaces
        .passwords_vault_invoke(
            &workspace,
            PASSWORDS_APP,
            KEEP_SECRET,
            arguments(json!({"id": "not an id", "part": "password", "secret": "hunter2"})),
        )
        .await
        .unwrap_err();
    assert!(!refused.contains("hunter2"), "{refused}");

    invoke(FORGET, json!({"id": ENTRY})).await.unwrap();
    let reading = workspaces
        .passwords_vault_reading(&workspace, PASSWORDS_APP)
        .await
        .unwrap();
    assert!(
        entry(&reading, ENTRY).is_none(),
        "a forgotten entry is gone"
    );
    assert_eq!(
        workspaces
            .passwords_vault_reading(&workspace, PASSWORDS_APP)
            .await
            .unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
