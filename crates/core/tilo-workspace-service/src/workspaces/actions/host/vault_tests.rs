//! A workspace's vault on its host: what a member keeps to sign in with is
//! shared by every member, listed only as far as it is safe to say, and
//! reached as the workspace's policy says at Passwords' vault. A member of
//! another workspace reaches none of it, and an agent its member's device
//! handed its session to reads it while its changes ask its person.

#![allow(
    clippy::too_many_lines,
    reason = "a vault's tests walk one subject each, whole"
)]

use std::sync::Arc;

use serde_json::{Value, json};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionCallError, ActionProvider as _, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};
use tilo_actions::remote::{model_providers, person_accounts, person_vault};
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;

use super::super::ServiceWorkspaceActions;
use super::super::tests::{admit, caller, governed, settled, signed_from};
use super::turn_use_tests::asks;
use crate::workspaces::Workspaces;
use crate::workspaces::fixtures::{test_host, test_host_holding};
use crate::workspaces::tests::{governor, guest_device};

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada@example.test";
/// What no site shows as a setup key: it holds digits a seed never does.
const NOT_A_SEED: &str = "correct horse battery staple 0189";
const KEY: &str = "sk-live-ada-own-key";

/// Who each change of `workspace_id`'s vault names as made for, in order: the people its
/// history entry names, and whether it keeps the call they signed.
async fn vault_history(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
) -> Vec<(Vec<String>, bool)> {
    let bytes = workspaces
        .resource_document(
            &workspace_id.to_string(),
            tilo_app_id::apps::PASSWORDS,
            tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
        )
        .await
        .unwrap()
        .expect("the workspace keeps its vault");
    let doc = automerge::Automerge::load(&bytes).unwrap();
    doc.get_changes(&[])
        .into_iter()
        .filter_map(|change| {
            let context = change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)?;
            let people = context.invoker.as_ref().map_or_else(Vec::new, |invoker| {
                invoker
                    .acting
                    .people()
                    .into_iter()
                    .map(str::to_owned)
                    .collect()
            });
            Some((people, context.signed.is_some()))
        })
        .collect()
}

/// `target` asked by `caller`, on the device `device` certifies, in the
/// workspace `workspace_id` carries.
pub(super) async fn ask_at(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    target: RemoteActionTarget,
    argument: Value,
) -> Result<Value, ActionCallError> {
    asked(
        service,
        workspace_id,
        (device, caller),
        &request_of(target, argument),
    )
    .await
}

/// The call of `target` with `argument`, under a fresh invocation id.
fn request_of(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
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
    }
}

/// `request` asked by `caller`, on the device `device` certifies, in the workspace
/// `workspace_id` carries: what it answered, or, where it waits for its person's yes, what they
/// are shown.
async fn asked(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    request: &InvokeRequest,
) -> Result<Value, ActionCallError> {
    let request = &signed_from(request, &workspace_id.to_string(), (device, caller));
    let receipt = settled(
        service
            .device_page(workspace_id, device)
            .invoke(caller, request.clone()),
    )
    .await?;
    super::waiting::at_once(service.shared.workspaces.get().unwrap(), request, receipt)
}

/// `action` of the vault, asked as [`ask_at`] asks.
pub(super) async fn ask(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    action: &str,
    argument: Value,
) -> Result<Value, ActionCallError> {
    ask_at(
        service,
        workspace_id,
        who,
        person_vault::target(action),
        argument,
    )
    .await
}

/// `target` asked by `caller` as [`ask_at`] asks, which waits for its person's yes, and then
/// approved by the person of `device`: what it answered.
async fn allowed_at(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    (device, caller): (&DeviceCert, &Caller),
    (target, argument): (RemoteActionTarget, Value),
) -> Result<Value, ActionCallError> {
    let request = request_of(target, argument);
    asks(asked(service, workspace_id, (device, caller), &request).await);
    super::waiting::answered_by(
        &service.device_page(workspace_id, device),
        &super::super::tests::caller(device),
        (caller, &request),
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await
}

/// The fields `said` carries, in order of their names.
fn fields(said: &Value) -> Vec<&str> {
    let mut fields: Vec<&str> = said
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    fields.sort_unstable();
    fields
}

/// A login to keep for the site `site`.
fn a_login(site: &str) -> Value {
    json!({"kind": "login", "belongsTo": [site], "username": NAME, "secret": PASSWORD})
}

/// Every file under `dir` whose bytes hold `needle`.
pub(super) fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// A workspace keeps what one member signs in with. What the server answers,
/// then and in every listing, is its kind, where it belongs and when it was
/// added, under a reference: never the secret, never the name. Every member
/// reads the same item; a member of another workspace and a person in none
/// are told there is no such action, and the host's disk holds the secret
/// nowhere in the clear.
#[tokio::test]
async fn a_members_login_is_the_workspaces_and_every_member_reads_only_what_is_safe() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));

    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://www.linkedin.com/login?session=1"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    assert!(
        person_vault::ItemRef::parse(&reference).is_some(),
        "{reference}"
    );
    assert_eq!(kept["kind"], "login");
    assert_eq!(kept["belongsTo"], json!(["https://www.linkedin.com"]));
    assert!(kept["addedAt"].as_u64().is_some_and(|added| added > 0));
    assert_eq!(
        kept["title"], "www.linkedin.com",
        "the item is picked by the title its entry carries"
    );
    assert_eq!(
        fields(&kept),
        [
            "addedAt",
            "belongsTo",
            "changedAt",
            "concerns",
            "kind",
            "reference",
            "title"
        ]
    );
    assert_eq!(
        kept["concerns"],
        json!(["weak"]),
        "a password with no capital and no digit is called weak"
    );

    // Every member of the workspace reads the same item, and none of them
    // reads more than what is safe to say of it.
    for (who, device, asking) in [("Ada", &ada, &as_ada), ("Bo", &bo, &as_bo)] {
        let theirs = ask(
            &service,
            workspace_id,
            (device, asking),
            person_vault::LIST,
            json!({}),
        )
        .await
        .unwrap();
        assert_eq!(theirs["items"], json!([kept]), "{who}");
        let said = theirs.to_string();
        assert!(
            !said.contains(PASSWORD) && !said.contains(NAME),
            "{who}: {said}"
        );
    }

    // A member of another workspace, and a person in none of them, are
    // nobody to this workspace's vault, whether they ask for it or change it.
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let (_, stranger) = guest_device();
    for (who, device, asking) in [
        ("a member of another workspace", &cy, &caller(&cy)),
        ("a non-member", &stranger, &caller(&stranger)),
    ] {
        let listed = ask(
            &service,
            workspace_id,
            (device, asking),
            person_vault::LIST,
            json!({}),
        )
        .await;
        assert!(
            matches!(listed, Err(ActionCallError::UnknownAction)),
            "{who}: {listed:?}"
        );
        let removed = ask(
            &service,
            workspace_id,
            (device, asking),
            person_vault::REMOVE,
            json!({"item": &reference}),
        )
        .await;
        assert!(
            matches!(removed, Err(ActionCallError::UnknownAction)),
            "{who}: {removed:?}"
        );
    }
    // The workspace they are in keeps no item of this one's.
    let theirs = ask(
        &service,
        elsewhere_id,
        (&cy, &caller(&cy)),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(theirs["items"], json!([]), "{theirs}");

    for needle in [PASSWORD, NAME, "linkedin"] {
        let clear = files_holding(dir.path(), needle.as_bytes());
        assert!(clear.is_empty(), "{needle} in the clear: {clear:?}");
    }
    assert!(
        workspaces
            .resource_document(
                &workspace_id.to_string(),
                tilo_app_id::apps::PASSWORDS,
                tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
            )
            .await
            .unwrap()
            .is_some(),
        "the item is sealed in the workspace's vault document"
    );

    // An editor's device takes it back for the whole workspace.
    let remaining = ask(
        &service,
        workspace_id,
        (&bo, &as_bo),
        person_vault::REMOVE,
        json!({"item": &reference}),
    )
    .await
    .expect("Bo takes it back");
    assert_eq!(remaining["items"], json!([]));

    // The vault's history says who did what: Ada kept the item and Bo took it back, each by
    // the call they signed, which the host that keeps the vault wrote down for them.
    let (ada_key, bo_key) = (
        hex::encode(ada.user_public_key),
        hex::encode(bo.user_public_key),
    );
    let made_for: Vec<_> = vault_history(workspaces, workspace_id)
        .await
        .into_iter()
        .filter(|(people, _)| people.contains(&ada_key) || people.contains(&bo_key))
        .collect();
    assert_eq!(
        made_for,
        [(vec![ada_key], true), (vec![bo_key], true)],
        "the vault's history names Ada's keeping and Bo's taking back"
    );
    server.shutdown().await;
}

/// What a person keeps beside a secret is the vault's too: an item's title, its note, the
/// fields they added and its tags travel in with its add. A listing names its title, its tags
/// and its fields' labels, and says whether it keeps a note; never the secret, the note or a
/// field's value. A password the vault draws itself is sent by nobody. None of it is in a log
/// line or a file in the clear.
#[tokio::test]
async fn an_items_title_note_fields_and_tags_are_the_vaults_and_list_without_secrets() {
    const NOTE: &str = "made-up-recovery-words-4471";
    const PIN: &str = "made-up-pin-7351";
    const STRONG: &str = "Made-up-Strong-Pass-0451";
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let (lines, logging) = super::web_call_tests::logged();

    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        json!({
            "kind": "login",
            "belongsTo": ["https://bank.example"],
            "username": NAME,
            "title": "Bank",
            "notes": NOTE,
            "fields": [{"label": "PIN", "value": PIN}],
            "tags": ["money", "family"],
            "secret": STRONG,
        }),
    )
    .await
    .expect("Ada keeps a login with a note, a field and tags");
    assert_eq!(kept["title"], "Bank");
    assert_eq!(kept["tags"], json!(["family", "money"]));
    assert_eq!(kept["fields"], json!(["PIN"]));
    assert_eq!(kept["hasNotes"], true);
    assert_eq!(
        kept.get("concerns"),
        None,
        "a strong password is no concern: {kept}"
    );

    // The same password kept again is reused, and both say so; a drawn one is neither weak
    // nor reused, and nobody sent it.
    ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        json!({"kind": "login", "belongsTo": ["https://shop.example"], "secret": STRONG}),
    )
    .await
    .expect("Ada keeps the same password for a shop");
    let drawn = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        json!({
            "kind": "login",
            "belongsTo": ["https://mail.example"],
            "title": "Mail",
            "generate": {"length": 24},
        }),
    )
    .await
    .expect("the vault draws a password for Ada's mail");
    for refused in [
        json!({"kind": "login", "belongsTo": ["https://a.example"], "generate": {}, "secret": "x"}),
        json!({"kind": "login", "belongsTo": ["https://a.example"], "generate": {"length": 8}}),
        json!({"kind": "key", "belongsTo": ["https://a.example"], "generate": {}}),
        json!({"kind": "login", "belongsTo": ["https://a.example"], "secret": "x",
               "fields": [{"label": "tilo.vault.kind", "value": "value"}]}),
    ] {
        let said = ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_vault::ADD,
            refused.clone(),
        )
        .await;
        assert!(
            matches!(said, Err(ActionCallError::Refused { .. })),
            "{refused}: {said:?}"
        );
    }

    let listed = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{listed}");
    let by = |reference: &Value| {
        items
            .iter()
            .find(|item| item["reference"] == *reference)
            .unwrap()
            .clone()
    };
    let mut bank = kept.clone();
    bank["concerns"] = json!(["reused"]);
    assert_eq!(by(&kept["reference"]), bank);
    let shop = items
        .iter()
        .find(|item| item["belongsTo"] == json!(["https://shop.example"]))
        .unwrap();
    assert_eq!(shop["concerns"], json!(["reused"]), "{listed}");
    let mail = by(&drawn["reference"]);
    assert_eq!(mail["title"], "Mail");
    assert_eq!(
        mail.get("concerns"),
        None,
        "a drawn password is no concern: {listed}"
    );

    let said = listed.to_string() + &kept.to_string() + &drawn.to_string();
    for secret in [STRONG, NOTE, PIN, NAME] {
        assert!(!said.contains(secret), "{secret} answered back: {said}");
    }
    drop(logging);
    let logged = String::from_utf8_lossy(&lines.0.lock().unwrap()).into_owned();
    for secret in [STRONG, NOTE, PIN] {
        assert!(!logged.contains(secret), "{secret} logged");
        let clear = files_holding(dir.path(), secret.as_bytes());
        assert!(clear.is_empty(), "{secret} in the clear: {clear:?}");
    }
    server.shutdown().await;
}

/// The workspace's vault is reached by its members as its policy says: an
/// agent its member's device handed its session to reads it, and every
/// change it would make asks its person first. A bot, an app, an agent no
/// device stands behind and the person's key on another person's device are
/// told what a caller asking for something that does not exist is told.
#[tokio::test]
async fn a_members_agent_reads_the_workspace_vault_and_its_changes_ask_its_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].clone();
    let entry = an_entry("Bank", PASSWORD);
    ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::KEEP_ENTRY,
        json!({"entry": entry}),
    )
    .await
    .expect("Ada keeps an entry");

    // Her agent, over the connection her device handed it, reads the
    // items and the entries.
    let agent = as_ada.clone().delegated_to("assistant");
    let listed = ask(
        &service,
        workspace_id,
        (&ada, &agent),
        person_vault::LIST,
        json!({}),
    )
    .await
    .expect("her agent reads the items");
    // One vault: her Passwords login for a site is an item by its own id, beside the one she
    // kept as an item.
    let entry_item = format!("vault.item:{}", entry["id"].as_str().unwrap());
    let items = listed["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 2, "{listed}");
    assert!(
        items.iter().any(|item| item["reference"] == reference),
        "{listed}"
    );
    assert!(
        items
            .iter()
            .any(|item| item["reference"] == json!(entry_item)),
        "{listed}"
    );
    let entries = ask(
        &service,
        workspace_id,
        (&ada, &agent),
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .expect("her agent reads the entries");
    assert_eq!(
        entries["entries"].as_array().map(Vec::len),
        Some(2),
        "the item she kept is an entry of Passwords too: {entries}"
    );

    // Every change waits for its person: with nobody to ask, their answer
    // is still wanted and nothing changed.
    for (action, argument) in [
        (person_vault::ADD, a_login("https://attacker.example")),
        (person_vault::REMOVE, json!({"item": &reference})),
        (
            person_vault::KEEP_ENTRY,
            json!({"entry": an_entry("Planted", "planted")}),
        ),
        (
            person_vault::FORGET_ENTRY,
            json!({"id": "an-entry-of-hers"}),
        ),
    ] {
        let refused = ask(&service, workspace_id, (&ada, &agent), action, argument).await;
        assert!(
            matches!(refused, Err(ActionCallError::ConfirmationRequired { .. })),
            "{action}: {refused:?}"
        );
    }
    let unchanged = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(unchanged["items"], listed["items"]);

    // Allowed by its person, the change it would make is made.
    allowed_at(
        &service,
        workspace_id,
        (&ada, &agent),
        (
            person_vault::target(person_vault::ADD),
            a_login("https://example.test"),
        ),
    )
    .await
    .expect("her agent keeps what she allowed");
    let now = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(now["items"].as_array().map(Vec::len), Some(3));

    // Nobody else reaches it, however it names itself.
    let person = hex::encode(ada.user_public_key);
    let not_her: [(&str, &DeviceCert, Caller); 4] = [
        ("a bot", &ada, Caller::local_bot("a-bot")),
        (
            "an app acting for her",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(person.clone()),
        ),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            Caller::local_agent("assistant").acting_for(person),
        ),
        ("her key on another person's device", &bo, caller(&ada)),
    ];
    for (who, device, asking) in &not_her {
        for (action, argument) in [
            (person_vault::LIST, json!({})),
            (person_vault::ADD, a_login("https://attacker.example")),
            (person_vault::REMOVE, json!({"item": &reference})),
            (person_vault::ENTRIES, json!({})),
            (
                person_vault::KEEP_ENTRY,
                json!({"entry": an_entry("Planted", "planted")}),
            ),
            (
                person_vault::FORGET_ENTRY,
                json!({"id": "an-entry-of-hers"}),
            ),
        ] {
            let refused = ask(&service, workspace_id, (device, asking), action, argument).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // Nothing of the workspace's moved.
    let still = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(still["items"].as_array().map(Vec::len), Some(3), "{still}");
    server.shutdown().await;
}

/// The vault is a record the workspace keeps apart: a grant on the whole workspace, to an agent
/// or to someone who is no member, reaches none of it. The agent's change still waits for its
/// person, and the non-member is told what a caller asking for something that does not exist is
/// told; nothing in the vault moves.
#[tokio::test]
async fn a_workspace_grant_writes_nothing_into_the_vault() {
    use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let agent = as_ada.clone().delegated_to("assistant");
    let (_, stranger) = guest_device();
    let owner_person = Grantee::person(hex::encode(owner.user_public_key));
    let everything = |id: &str, to: Grantee| {
        Grant::new(
            id,
            to,
            TreePath::workspace(&workspace),
            KindSet::ALL,
            Answer::Allow,
            owner_person.clone(),
        )
    };
    workspaces
        .put_workspace_grants(
            &workspace,
            &owner,
            [62; 32],
            vec![
                everything("test:agent-everywhere", Grantee::agent("assistant")),
                everything(
                    "test:stranger-everywhere",
                    Grantee::person(hex::encode(stranger.user_public_key)),
                ),
            ],
        )
        .await
        .unwrap();

    for (action, argument) in [
        (person_vault::ADD, a_login("https://attacker.example")),
        (person_vault::REMOVE, json!({"item": kept["reference"]})),
        (
            person_vault::KEEP_ENTRY,
            json!({"entry": an_entry("Planted", "planted")}),
        ),
    ] {
        let by_agent = ask(
            &service,
            workspace_id,
            (&ada, &agent),
            action,
            argument.clone(),
        )
        .await;
        assert!(
            matches!(by_agent, Err(ActionCallError::ConfirmationRequired { .. })),
            "the agent, {action}: {by_agent:?}"
        );
        let by_stranger = ask(
            &service,
            workspace_id,
            (&stranger, &caller(&stranger)),
            action,
            argument,
        )
        .await;
        assert!(
            matches!(by_stranger, Err(ActionCallError::UnknownAction)),
            "a non-member, {action}: {by_stranger:?}"
        );
    }
    let still = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(still["items"], json!([kept]), "{still}");
    server.shutdown().await;
}

/// An entry of the person's Passwords: what the app sends, secret included.
fn an_entry(title: &str, password: &str) -> Value {
    let mut entry = accounts::SecretItem::login(title, NAME, password);
    entry.add_origin("https://example.com").unwrap();
    serde_json::to_value(entry).unwrap()
}

/// The workspace keeps the entries of its Passwords: a member keeps one
/// from their device, every member reads them back secrets included, edits
/// one in place and forgets it. A member of another workspace reads none of
/// it, there or here.
#[tokio::test]
async fn the_workspaces_passwords_are_kept_and_read_by_its_members_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let hers = (&ada, &caller(&ada));
    let entries = |answer: &Value| answer["entries"].as_array().cloned().unwrap_or_default();

    let entry = an_entry("Bank", PASSWORD);
    let kept = ask(
        &service,
        workspace_id,
        hers,
        person_vault::KEEP_ENTRY,
        json!({"entry": entry}),
    )
    .await
    .expect("Ada keeps an entry");
    assert_eq!(entries(&kept).len(), 1, "{kept}");
    let read = ask(
        &service,
        workspace_id,
        hers,
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .expect("Ada reads her entries");
    let read = entries(&read);
    assert_eq!(read.len(), 1);
    assert_eq!(read[0]["title"], "Bank");
    assert_eq!(
        read[0]["password"], PASSWORD,
        "the workspace's secret comes back to its member"
    );

    let mut edited = read[0].clone();
    edited["title"] = json!("Bank, renamed");
    let after = ask(
        &service,
        workspace_id,
        hers,
        person_vault::KEEP_ENTRY,
        json!({"entry": edited}),
    )
    .await
    .unwrap();
    let after = entries(&after);
    assert_eq!(after.len(), 1, "edited in place");
    assert_eq!(after[0]["title"], "Bank, renamed");

    // Every member reads the entries of the workspace's Passwords.
    let his = ask(
        &service,
        workspace_id,
        (&bo, &caller(&bo)),
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(entries(&his), after, "Bo reads the workspace's entries");

    // A member of another workspace reads none of it, there or here.
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    let refused = ask(
        &service,
        workspace_id,
        (&cy, &caller(&cy)),
        person_vault::ENTRIES,
        json!({}),
    )
    .await;
    assert!(
        matches!(refused, Err(ActionCallError::UnknownAction)),
        "{refused:?}"
    );
    let theirs = ask(
        &service,
        elsewhere_id,
        (&cy, &caller(&cy)),
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(theirs["entries"], json!([]), "{theirs}");

    // One vault: her login for a site is an item an agent may name, by the entry's own id.
    let items = ask(&service, workspace_id, hers, person_vault::LIST, json!({}))
        .await
        .unwrap();
    assert_eq!(
        items["items"][0]["reference"],
        json!(format!("vault.item:{}", after[0]["id"].as_str().unwrap())),
        "{items}"
    );
    assert_eq!(items["items"].as_array().map(Vec::len), Some(1), "{items}");

    let id = after[0]["id"].clone();
    let left = ask(
        &service,
        workspace_id,
        hers,
        person_vault::FORGET_ENTRY,
        json!({"id": id}),
    )
    .await
    .unwrap();
    assert_eq!(left["entries"], json!([]));
    let again = ask(
        &service,
        workspace_id,
        hers,
        person_vault::FORGET_ENTRY,
        json!({"id": id}),
    )
    .await;
    assert!(again.is_ok(), "one gone already is no refusal: {again:?}");
    server.shutdown().await;
}

/// An item that is not one to keep is refused before anything is kept, and
/// the refusal never repeats what was typed: a secret put where a place or a
/// kind belongs is not echoed back.
#[tokio::test]
async fn an_item_that_is_not_one_is_refused_without_repeating_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);

    for unusable in [
        json!({"kind": "login", "belongsTo": [PASSWORD], "secret": PASSWORD}),
        json!({"kind": PASSWORD, "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": PASSWORD, "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": [PASSWORD]}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "password": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": ""}),
        json!({"kind": "login", "belongsTo": [], "secret": PASSWORD}),
        json!({"kind": "ssh-key", "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "code-generator", "belongsTo": ["https://example.com"], "secret": NOT_A_SEED}),
    ] {
        let refused = ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            person_vault::ADD,
            unusable.clone(),
        )
        .await
        .expect_err("an unusable item is refused");
        assert!(
            matches!(
                refused,
                ActionCallError::Refused { .. } | ActionCallError::InvalidRequest { .. }
            ),
            "{unusable}: {refused:?}"
        );
        let said = format!("{refused} {refused:?}");
        assert!(!said.contains(PASSWORD) && !said.contains("0189"), "{said}");
    }
    let refused = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::REMOVE,
        json!({"item": PASSWORD}),
    )
    .await
    .expect_err("what is no reference names nothing");
    assert!(!format!("{refused} {refused:?}").contains(PASSWORD));

    let hers = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([]));
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// A provider of whoever asks, at `https://models.example/v1`, by its id.
pub(super) async fn a_provider(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> String {
    let added = ask_at(
        service,
        workspace_id,
        who,
        model_providers::target(model_providers::ADD_PROVIDER),
        json!({"name": "Models", "protocol": "open-ai-compatible", "endpoint": "https://models.example/v1"}),
    )
    .await
    .expect("a provider is added");
    added["id"].as_str().expect("the provider's id").to_owned()
}

/// A `kind` kept for `place` in the vault of whoever asks, by its reference.
pub(super) async fn kept_item(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place): (&str, &str),
) -> Value {
    let kept = ask(
        service,
        workspace_id,
        who,
        person_vault::ADD,
        json!({"kind": kind, "belongsTo": [place], "secret": KEY}),
    )
    .await
    .expect("an item is kept");
    kept["reference"].clone()
}

/// The uses the item `item` names remembers, as its person reads them.
async fn uses_of(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    item: &Value,
) -> Value {
    let uses = ask(
        service,
        workspace_id,
        who,
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("a person reads what their item was used for");
    uses["uses"].clone()
}

/// An agent acting for a member keeps a provider's key from the workspace's
/// vault by naming the item. The host resolves it: the provider is
/// configured once its person allowed the change, the item remembers when,
/// where and for whom, and nothing the agent is answered carries the key.
/// What the item was used for is a member's to read, and taking a leave back
/// is not an agent's.
#[tokio::test]
async fn an_agent_keeps_a_providers_key_by_naming_the_vault_item() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let provider = a_provider(&service, workspace_id, (&ada, &as_ada)).await;
    let item = kept_item(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("key", "https://models.example/v1/chat"),
    )
    .await;
    // The agent her own device hands its session to: a certificate delegates to no agent it
    // does not hand a session or bind to a roster.
    let helper = as_ada.clone().delegated_to("assistant").named("Helper");

    // A change the agent makes is put to its person first: with nobody to
    // ask, nothing is kept.
    let keeping = json!({"provider": provider, "key": item});
    let unasked = ask_at(
        &service,
        workspace_id,
        (&ada, &helper),
        model_providers::target(model_providers::SET_KEY_FROM_ITEM),
        keeping.clone(),
    )
    .await;
    assert!(
        matches!(unasked, Err(ActionCallError::ConfirmationRequired { .. })),
        "{unasked:?}"
    );
    let view = allowed_at(
        &service,
        workspace_id,
        (&ada, &helper),
        (
            model_providers::target(model_providers::SET_KEY_FROM_ITEM),
            keeping,
        ),
    )
    .await
    .expect("the agent keeps the key by naming its item, once its person allowed it");
    assert_eq!(view["configured"], json!(true), "{view}");
    assert!(!view.to_string().contains(KEY), "{view}");

    // The provider names the very item: its key is the entry the vault
    // keeps, copied nowhere, and nowhere in the clear.
    let own = workspaces.workspace_root(&workspace_id.to_string());
    let named = provider_named(&own, &provider).expect("the provider names its key");
    assert_eq!(json!(named.to_string()), item);
    assert_eq!(
        entry_of(workspaces, workspace_id, &named).await.as_deref(),
        Some(KEY)
    );
    assert!(files_holding(dir.path(), KEY.as_bytes()).is_empty());

    let listed = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert!(
        listed["items"][0]["lastUsedAt"].as_u64().is_some(),
        "{listed}"
    );
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &item).await;
    assert_eq!(uses.as_array().map(Vec::len), Some(1), "{uses}");
    assert_eq!(uses[0]["places"], json!(["https://models.example"]));
    assert_eq!(uses[0]["by"], "Helper");
    assert_eq!(uses[0]["using"], "tilo.models/set-key-from-item");
    assert_eq!(fields(&uses[0]), ["at", "by", "places", "using"]);
    // What the item was used for is a member's to read, and the agent reads
    // it with its person's access: only taking a leave back is not its own.
    let read = ask(
        &service,
        workspace_id,
        (&ada, &helper),
        person_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("the agent reads what its item remembers");
    assert_eq!(read["uses"], uses);
    let taken = ask(
        &service,
        workspace_id,
        (&ada, &helper),
        person_vault::TAKE_BACK,
        json!({"item": item}),
    )
    .await;
    assert!(
        matches!(taken, Err(ActionCallError::UnknownAction)),
        "{taken:?}"
    );
    server.shutdown().await;
}

/// A provider of whoever asks and three items of theirs a use might name:
/// a key for another service, a login for the provider's site, and the
/// provider's own key.
async fn a_provider_and_items(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
) -> (String, [Value; 3]) {
    let provider = a_provider(service, workspace_id, who).await;
    let elsewhere = kept_item(service, workspace_id, who, ("key", "https://other.example")).await;
    let login = kept_item(
        service,
        workspace_id,
        who,
        ("login", "https://models.example"),
    )
    .await;
    let right = kept_item(
        service,
        workspace_id,
        who,
        ("key", "https://models.example"),
    )
    .await;
    (provider, [elsewhere, login, right])
}

/// Nothing was kept and nothing used: the provider of `who` has no key and
/// none of `items` remembers a use.
async fn nothing_was_used(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    items: &[Value; 3],
) {
    let models = ask_at(
        service,
        workspace_id,
        who,
        model_providers::target(model_providers::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(
        models["providers"][0]["configured"],
        json!(false),
        "{models}"
    );
    for item in items {
        assert_eq!(uses_of(service, workspace_id, who, item).await, json!([]));
    }
}

/// A reference is used only where its item belongs, as the kind the use
/// needs, in the workspace whose vault holds it: the wrong place, the wrong
/// kind and another workspace's vault are each refused, and nothing is used.
#[tokio::test]
async fn a_reference_is_refused_wherever_its_item_does_not_belong() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let (elsewhere, elsewhere_id, _) = governed(workspaces).await;
    let bo = admit(workspaces, &elsewhere, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&service, workspace_id, hers).await;
    let [elsewhere_item, login, right] = &items;
    let his_provider = a_provider(&service, elsewhere_id, (&bo, &as_bo)).await;

    for (who, workspace, provider, item, reason) in [
        (
            hers,
            workspace_id,
            &provider,
            elsewhere_item,
            "That item belongs somewhere else",
        ),
        (
            hers,
            workspace_id,
            &provider,
            login,
            "That item is not the kind of thing this needs",
        ),
        (
            (&bo, &as_bo),
            elsewhere_id,
            &his_provider,
            right,
            "That item is not kept here",
        ),
        (
            (&bo, &as_bo),
            elsewhere_id,
            &provider,
            right,
            "That provider is not kept here",
        ),
    ] {
        let refused = ask_at(
            &service,
            workspace,
            who,
            model_providers::target(model_providers::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": item}),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason: said }) if said == reason),
            "{reason}: {refused:?}"
        );
    }
    nothing_was_used(&service, workspace_id, hers, &items).await;
    server.shutdown().await;
}

/// A bot, an app and an agent no certificate stands behind use nothing of
/// the workspace's vault, and an agent's typed key asks its person before
/// anything is kept: that way stays the person's own.
#[tokio::test]
async fn a_members_agent_uses_the_workspaces_vault_only_by_reference() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let (provider, items) = a_provider_and_items(&service, workspace_id, hers).await;
    let [_, _, right] = &items;
    let from_item = model_providers::target(model_providers::SET_KEY_FROM_ITEM);
    let person = hex::encode(ada.user_public_key);

    for (other, target, key) in [
        (Caller::local_bot("a-bot"), from_item.clone(), right.clone()),
        (
            Caller::local_app("so.tilo.chat").acting_for(person.clone()),
            from_item.clone(),
            right.clone(),
        ),
        (
            Caller::local_agent("assistant").acting_for(person),
            from_item,
            right.clone(),
        ),
    ] {
        let refused = ask_at(
            &service,
            workspace_id,
            (&ada, &other),
            target,
            json!({"provider": provider, "key": key}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{other:?}: {refused:?}"
        );
    }

    // An agent of hers hands no typed key in: keeping one by typing it asks
    // its person first, and nobody answers.
    let helper = as_ada.clone().delegated_to("assistant");
    let typed = ask_at(
        &service,
        workspace_id,
        (&ada, &helper),
        model_providers::target(model_providers::SET_KEY),
        json!({"provider": provider, "key": KEY}),
    )
    .await;
    assert!(
        matches!(typed, Err(ActionCallError::ConfirmationRequired { .. })),
        "{typed:?}"
    );
    nothing_was_used(&service, workspace_id, hers, &items).await;
    server.shutdown().await;
}

/// A mail sign-in named by reference goes only to the servers its item
/// belongs to: an account at any other server or port is refused before a
/// server is reached, and a password typed in by an agent asks its person
/// first.
#[tokio::test]
async fn a_mail_sign_in_by_reference_goes_only_to_its_own_servers() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let kept = ask(
        &service,
        workspace_id,
        hers,
        person_vault::ADD,
        json!({
            "kind": "login",
            "belongsTo": ["imap://imap.example.test:993", "smtp://smtp.example.test:465"],
            "username": NAME,
            "secret": PASSWORD,
        }),
    )
    .await
    .expect("Ada keeps her mail sign-in");
    let sign_in = kept["reference"].clone();
    let with_item = person_accounts::target(person_accounts::ADD_MAIL_WITH_ITEM);

    for (incoming, port, outgoing) in [
        ("imap.attacker.example", 993, "smtp.example.test"),
        ("imap.example.test", 993, "smtp.attacker.example"),
        ("imap.example.test", 143, "smtp.example.test"),
    ] {
        let refused = ask_at(
            &service,
            workspace_id,
            hers,
            with_item.clone(),
            json!({
                "address": NAME,
                "incomingHost": incoming,
                "incomingPort": port,
                "outgoingHost": outgoing,
                "password": sign_in,
            }),
        )
        .await;
        assert!(
            matches!(&refused, Err(ActionCallError::Refused { reason }) if reason == "That item belongs somewhere else"),
            "{incoming}:{port} {outgoing}: {refused:?}"
        );
    }
    // A password typed in by an agent waits for its person first.
    let helper = as_ada.clone().delegated_to("assistant");
    let typed = ask_at(
        &service,
        workspace_id,
        (&ada, &helper),
        person_accounts::target(person_accounts::ADD_MAIL),
        json!({
            "address": NAME,
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": PASSWORD,
        }),
    )
    .await;
    assert!(
        matches!(typed, Err(ActionCallError::ConfirmationRequired { .. })),
        "{typed:?}"
    );

    let accounts = ask_at(
        &service,
        workspace_id,
        hers,
        person_accounts::target(person_accounts::LIST),
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(accounts["accounts"], json!([]), "{accounts}");
    assert_eq!(
        uses_of(&service, workspace_id, hers, &sign_in).await,
        json!([])
    );
    server.shutdown().await;
}

/// The entry of the workspace's vault the provider `provider` of the store at `root` names
/// as its key.
fn provider_named(root: &std::path::Path, provider: &str) -> Option<person_vault::ItemRef> {
    crate::workspaces::person_settings::provider_config(root)
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .and_then(crate::workspaces::person_settings::key_reference)
}

/// The entry of the workspace's vault the account `account` of the workspace `workspace` names
/// as its sign-in.
async fn account_named(
    workspaces: &Workspaces,
    workspace: &str,
    account: &str,
) -> Option<person_vault::ItemRef> {
    workspaces
        .workspace_accounts(workspace)
        .await
        .unwrap()
        .account(account)
        .and_then(|kept| {
            accounts::email::password_item(&kept.config).and_then(person_vault::ItemRef::parse)
        })
}

/// What the entry `named` of the workspace's vault holds.
async fn entry_of(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
    named: &person_vault::ItemRef,
) -> Option<String> {
    workspaces
        .open_vault(&workspace_id.to_string())
        .await
        .unwrap()
        .entry(named.id())
        .map(|entry| entry.password.clone())
}

/// A mail account of `address` at `server`, as a host kept one.
fn mail_account(address: &str, server: &str) -> accounts::Account {
    let mut account = accounts::Account::new(address, accounts::email::TYPE_ID);
    account.config = json!({
        "email": address,
        "imap_host": format!("imap.{server}"),
        "imap_port": 993,
        "smtp_host": format!("smtp.{server}"),
        "smtp_port": 465,
    });
    account
}

/// What a host kept of a workspace's vault in its own sealed store before the vault was the
/// document, a member's login with the uses it recorded and a Passwords entry, is in the vault
/// document once the host carries it over as it starts: listed under the reference it had, its uses
/// kept, and taken out of the host's store. An account's sign-in beside it becomes the
/// account's own entry, and the host's store is gone.
#[cfg(feature = "device-keys")]
#[tokio::test]
async fn what_a_host_kept_of_a_vault_before_is_carried_into_the_vault_document() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let hers = (&ada, &caller(&ada));

    // The host's store, as a host before kept it.
    let root = workspaces.workspace_root(&workspace_id.to_string());
    let id = "7".repeat(32);
    let mut vault = accounts::SecretVault::open_person(Some(&root));
    let sealed = json!({
        "kind": "login",
        "places": ["https://example.test"],
        "username": NAME,
        "secret": PASSWORD,
        "added_unix": 1_700_000_000_u64,
        "uses": [{"at_unix": 1_700_000_100_u64, "places": ["https://example.test"], "by": "Assistant"}],
    });
    vault.put(
        &accounts::AccountId::new(format!("tilo-vault-item:{id}")),
        &accounts::Secret {
            password: String::new(),
            token: Some(sealed.to_string()),
        },
    );
    vault.put(
        &accounts::AccountId::new("ada@example.test"),
        &accounts::Secret::password("an account's own"),
    );
    vault.try_save().unwrap();
    workspaces
        .keep_workspace_account(
            (
                &workspace_id.to_string(),
                &workspaces.acting_or_own(None),
                None,
            ),
            &mail_account("ada@example.test", "example.test"),
        )
        .await
        .unwrap();
    let mut store = accounts::SecretStore::open_person(Some(&root));
    let mut entry = accounts::SecretItem::login("Example", "bo", "a passwords entry");
    store.put(&mut entry).unwrap();
    // What the host does once as it starts.
    workspaces.carry_vaults_over().await;

    let listed = ask(&service, workspace_id, hers, person_vault::LIST, json!({}))
        .await
        .unwrap();
    // The item the host kept, and the account's sign-in, an entry of the vault like any other.
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 2, "{listed}");
    let carried = items
        .iter()
        .find(|item| item["reference"] == json!(format!("vault.item:{id}")))
        .unwrap_or_else(|| panic!("the kept item is listed: {listed}"));
    assert_eq!(carried["belongsTo"], json!(["https://example.test"]));
    let sign_in = items
        .iter()
        .find(|item| item["reference"] != carried["reference"])
        .unwrap();
    assert_eq!(
        sign_in["belongsTo"],
        json!([
            "imap://imap.example.test:993",
            "smtp://smtp.example.test:465"
        ]),
        "{listed}"
    );
    let uses = ask(
        &service,
        workspace_id,
        hers,
        person_vault::USES,
        json!({"item": format!("vault.item:{id}")}),
    )
    .await
    .unwrap();
    assert_eq!(uses["uses"].as_array().map(Vec::len), Some(1), "{uses}");
    let entries = ask(
        &service,
        workspace_id,
        hers,
        person_vault::ENTRIES,
        json!({}),
    )
    .await
    .unwrap();
    assert!(
        entries.to_string().contains(&entry.id.0),
        "the Passwords entry is in the vault: {entries}"
    );

    // The host keeps no second copy: the account's sign-in is its own entry,
    // and the host's store is gone.
    assert!(
        accounts::vault_items::KeptBefore::under(&root)
            .unwrap()
            .is_empty()
    );
    let named = account_named(workspaces, &workspace_id.to_string(), "ada@example.test")
        .await
        .expect("the account names its entry");
    assert_eq!(
        entry_of(workspaces, workspace_id, &named).await.as_deref(),
        Some("an account's own")
    );
    assert!(
        !accounts::vault_dir_in(&root).exists(),
        "the host's store is gone"
    );
    server.shutdown().await;
}

/// Each sign-in a host kept in its own sealed store before every secret was the workspace
/// vault's, a mail account's password, a mail account's grant and a model provider's key, is
/// carried into the vault as the host starts: each account and provider names an entry of its
/// own, which holds what the store held and is what it signs in with, and the store is gone,
/// so the host keeps no copy anywhere.
#[tokio::test]
async fn the_sign_ins_a_host_kept_are_the_vaults_once_it_starts() {
    const GRANT: &str = "an-access-token-only-the-grant-holds";
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    // Its key service keeps the workspace's document keys across a restart, as a server's does:
    // the workspace's accounts are sealed for them.
    let keys = Arc::new(crate::workspaces::fixtures::TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = server.workspaces().clone();
    let (_, workspace_id, _) = governed(&workspaces).await;
    let workspace = workspace_id.to_string();
    // Its vault opened here before, so the host holds its keys as it starts, and its accounts
    // are the workspace's.
    workspaces.open_vault(&workspace).await.unwrap();
    let (mail, google) = (
        mail_account(NAME, "example.test"),
        mail_account("ada@gmail.example", "gmail.example"),
    );
    for account in [&mail, &google] {
        workspaces
            .keep_workspace_account((&workspace, &workspaces.acting_or_own(None), None), account)
            .await
            .unwrap();
    }
    server.shutdown().await;

    // The store the host kept, as a host before kept it.
    let root = workspaces.workspace_root(&workspace);
    let mut credentials = accounts::SecretVault::open_person(Some(&root));
    credentials.put_for(&mail, &accounts::Secret::password(PASSWORD));
    credentials.put_for(
        &google,
        &account_credentials::oauth_secret(&tilo_oauth::TokenSet {
            access_token: GRANT.to_owned(),
            scope: None,
            refresh_token: "a-refresh-token".to_owned(),
            expires_in: 3600,
            obtained_at: tilo_clock::unix_now(),
        }),
    );
    let mut profile = agent_provider_config::ProviderProfile::hosted(
        agent_provider_config::ProviderProtocol::OpenAiCompatible,
    );
    profile.endpoint = "https://models.example/v1".to_owned();
    let provider = profile.id.clone();
    crate::workspaces::person_settings::save_provider_config(
        &root,
        &agent_provider_config::ProviderConfig {
            profiles: vec![profile.clone()],
            ..agent_provider_config::ProviderConfig::default()
        },
    )
    .unwrap();
    // A provider's key, as the model service sealed it: under the service, what it is for and
    // the provider, beside the origin it was typed for.
    let sealed_as = serde_json::to_vec(&("model-gateway", "provider-api-key", &provider)).unwrap();
    credentials.put(
        &accounts::AccountId::new(format!(
            "tilo-service-credential:{}",
            hex::encode(sealed_as)
        )),
        &accounts::Secret {
            password: String::new(),
            token: Some(
                json!({
                    "authorization": {
                        "service": "model-gateway",
                        "purpose": "provider-api-key",
                        "origin": "https://models.example",
                    },
                    "value": KEY,
                })
                .to_string(),
            ),
        },
    );
    credentials.try_save().unwrap();

    // The host starts again over the same state and with the same key service, and carries them
    // over.
    let server = test_host_holding(dir.path(), &key, &owner, keys).await;
    let workspaces = server.workspaces();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while accounts::vault_dir_in(&root).exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the host's store is still there"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    let password = account_named(workspaces, &workspace, NAME)
        .await
        .expect("the mail account names its entry");
    assert_eq!(
        entry_of(workspaces, workspace_id, &password)
            .await
            .as_deref(),
        Some(PASSWORD)
    );
    let signing_in =
        crate::workspaces::person_settings::account_secret_for_test(workspaces, &workspace, NAME)
            .await
            .expect("the account signs in with its entry");
    assert_eq!(signing_in.password, PASSWORD);

    let grant = account_named(workspaces, &workspace, "ada@gmail.example")
        .await
        .expect("the grant's account names it");
    let grant = entry_of(workspaces, workspace_id, &grant)
        .await
        .expect("the grant is an entry");
    let tokens = account_credentials::oauth_tokens(&accounts::Secret {
        password: String::new(),
        token: Some(grant),
    })
    .expect("the entry is the grant");
    assert_eq!(tokens.access_token, GRANT);

    let key_named = provider_named(&root, &provider).expect("the provider names its key");
    assert_eq!(
        entry_of(workspaces, workspace_id, &key_named)
            .await
            .as_deref(),
        Some(KEY)
    );

    for secret in [PASSWORD, GRANT, KEY] {
        assert_eq!(
            files_holding(dir.path(), secret.as_bytes()),
            Vec::<std::path::PathBuf>::new(),
            "the host keeps a copy of {secret}"
        );
    }
    server.shutdown().await;
}

/// A workspace whose policy keeps Passwords from the host that serves it keeps its sign-ins
/// from that host too: the host holds no key of the vault, so an account that names an entry
/// of it no longer signs in there, and the host kept no copy of the sign-in to fall back on.
#[tokio::test]
async fn a_host_kept_from_passwords_signs_in_with_none_of_the_vaults_entries() {
    use tilo_server_protocol::workspaces::{WorkspacesReply, WorkspacesRequest};
    const KEPT: &str = "kept-for-a-test";

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    // Ada's workspace, which the host founded for her and serves.
    let ada = guest_device().1;
    let workspace = workspaces
        .create_for("Ada's", None, ada.user_public_key)
        .await
        .unwrap()
        .workspace;
    let signing_in = async || {
        crate::workspaces::person_settings::account_secret_for_test(workspaces, &workspace, NAME)
            .await
    };

    // An account names an entry of the vault, and signs in with it while the host reads
    // Passwords.
    workspaces
        .keep_mail_account_as_added(&workspace, NAME)
        .await
        .unwrap();
    assert_eq!(
        signing_in()
            .await
            .expect("the host reads the vault")
            .password,
        KEPT
    );

    // The vault is kept from the host's own person, who administers the workspace for her.
    let reply = workspaces
        .page_workspaces(
            &owner,
            WorkspacesRequest::SetDocumentAccess {
                workspace: workspace.clone(),
                request_id: [9; 32],
                app: tilo_app_id::apps::PASSWORDS.into(),
                document: tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT.into(),
                recipient: format!("member:{}", hex::encode(owner.user_public_key)),
                access: "no-access".into(),
            },
        )
        .await;
    assert!(matches!(reply, WorkspacesReply::Workspace(_)), "{reply:?}");

    let refused = signing_in().await;
    assert!(refused.is_err(), "the host still signs in: {refused:?}");
    assert!(
        !accounts::vault_dir_in(&workspaces.workspace_root(&workspace)).exists(),
        "the host keeps a vault of its own"
    );
    assert_eq!(
        files_holding(dir.path(), KEPT.as_bytes()),
        Vec::<std::path::PathBuf>::new(),
        "the host keeps a copy of the sign-in"
    );
    server.shutdown().await;
}

/// The models' reading and their edits are one resource's: a provider added reads as itself,
/// its key kept and cleared reads as configured or not, stopping it offering its models reads
/// as such, and removing it takes it out of the reading and its key out of the vault. The key
/// travels in and is in no reading, no listing and no file the workspace keeps.
#[tokio::test]
async fn a_models_key_is_kept_and_cleared_as_the_reading_says() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (_workspace, workspace_id, service) = governed(workspaces).await;
    // The person managing their own workspace's providers, on a device of their own: the vault
    // they write is theirs, and the models they choose are the workspace's.
    let ada = owner.clone();
    let as_ada = caller(&ada);
    let who = (&ada, &as_ada);
    let provider = a_provider(&service, workspace_id, who).await;
    let reading = || workspaces.models_reading(&workspace_id.to_string());
    let listed = |reading: &Value| {
        reading["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|shown| shown["id"] == json!(provider))
            .cloned()
    };

    let shown = listed(&reading()).expect("the provider the person added is listed");
    assert_eq!(shown["configured"], json!(false), "no key is kept yet");
    assert_eq!(shown["enabled"], json!(true));

    let kept = ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::SET_KEY),
        json!({"provider": provider, "key": KEY}),
    )
    .await
    .expect("the person keeps their key");
    assert_eq!(kept["configured"], json!(true), "{kept}");
    assert!(!kept.to_string().contains(KEY), "the key never reads back");
    let shown = listed(&reading()).expect("the provider is listed");
    assert_eq!(shown["configured"], json!(true), "{shown}");
    assert!(
        !reading().to_string().contains(KEY) && !shown.to_string().contains(KEY),
        "no reading carries a key"
    );
    assert!(
        files_holding(dir.path(), KEY.as_bytes()).is_empty(),
        "the key is sealed in the workspace's vault, in no file the workspace keeps"
    );

    let cleared = ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::REMOVE_KEY),
        json!({"provider": provider}),
    )
    .await
    .expect("the person clears it");
    assert_eq!(cleared["configured"], json!(false), "{cleared}");
    let shown = listed(&reading()).expect("the provider is listed");
    assert_eq!(shown["configured"], json!(false), "{shown}");

    let stopped = ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::SET_ENABLED),
        json!({"provider": provider, "enabled": false}),
    )
    .await
    .expect("the person stops offering it");
    assert_eq!(stopped["enabled"], json!(false), "{stopped}");
    let shown = listed(&reading()).expect("the provider is listed");
    assert_eq!(shown["enabled"], json!(false), "{shown}");

    // A provider the person stopped cannot be the one agents answer with.
    let refused = ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::SET_DEFAULT),
        json!({"provider": provider, "model": "any"}),
    )
    .await;
    assert!(
        matches!(
            &refused,
            Err(ActionCallError::Refused { reason }) if reason == "That provider is not offered to agents"
        ),
        "a stopped provider is no default: {refused:?}"
    );

    ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::SET_KEY),
        json!({"provider": provider, "key": KEY}),
    )
    .await
    .expect("its key is kept again");
    let left = ask_at(
        &service,
        workspace_id,
        who,
        model_providers::target(model_providers::REMOVE_PROVIDER),
        json!({"provider": provider}),
    )
    .await
    .expect("the person removes it");
    assert_eq!(left["providers"], json!([]), "it is gone from the reading");
    assert!(
        files_holding(dir.path(), KEY.as_bytes()).is_empty(),
        "its key went with it"
    );
    server.shutdown().await;
}
/// The seed RFC 6238 tests with, the bytes `12345678901234567890`, as a setup key would show it.
const SEED: &str = "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ";

/// A code generator's seed is kept in the vault, and the code of this moment is read where the
/// seed is: the answer says the digits and how long they stand, neither the seed nor a code is
/// written anywhere, and reading one is no use its person reads.
#[tokio::test]
async fn a_code_generators_code_reads_where_its_seed_is() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        json!({
            "kind": "code-generator",
            "belongsTo": [],
            "secret": SEED,
            "algorithm": "SHA256",
            "digits": 8,
            "period": 60,
        }),
    )
    .await
    .expect("a seed is kept");
    let item: person_vault::ItemRef = serde_json::from_value(kept["reference"].clone())
        .expect("the item is named by its reference");

    let said = ask_at(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::code_target(&item),
        json!({}),
    )
    .await
    .expect("the code of this moment reads where the seed is");
    let code = said["code"].as_str().expect("a code");
    assert_eq!(code.len(), 8, "{said}");
    assert!(code.bytes().all(|byte| byte.is_ascii_digit()), "{said}");
    assert_eq!(said["digits"], json!(8), "{said}");
    assert_eq!(said["period"], json!(60), "{said}");
    assert_eq!(said["algorithm"], json!("SHA256"), "{said}");
    assert!(
        said["left"].as_u64().expect("how long it stands") <= 60,
        "{said}"
    );
    assert!(
        !said.to_string().contains(SEED),
        "the seed is in no answer: {said}"
    );
    assert_eq!(
        uses_of(&service, workspace_id, (&ada, &as_ada), &kept["reference"]).await,
        json!([]),
        "reading a code is no use"
    );
}

/// A code generator kept without naming its hash, its digits or how long a code stands is kept
/// as a setup key that says none of them means: SHA1, 6 digits, 30 seconds.
#[tokio::test]
async fn a_code_generator_that_names_no_hash_makes_its_codes_with_sha1() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &caller(&ada)),
        person_vault::ADD,
        json!({"kind": "code-generator", "belongsTo": [], "secret": SEED}),
    )
    .await
    .expect("a seed that names no hash is kept");
    assert_eq!(kept["algorithm"], json!("SHA1"), "{kept}");
    assert_eq!(kept["digits"], json!(6), "{kept}");
    assert_eq!(kept["period"], json!(30), "{kept}");
    server.shutdown().await;
}

/// What the workspace's host writes in its vault for an agent's call is a step that call
/// declares, made under its person's yes, named as the agent's for her, and keeping the call
/// with her yes. A step the call does not declare is refused, and the vault keeps it nowhere.
#[tokio::test]
async fn an_agents_vault_write_is_the_allowed_calls_declared_step_alone() {
    use accounts::vault_items::{UseRecord, VaultChange};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        person_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let hers = hex::encode(ada.user_public_key);
    let agent = tilo_action_policy::Acting::agent("assistant")
        .acting_for(tilo_action_policy::Acting::person(hers.clone()));
    let fill = request_of(
        person_vault::page_target(),
        json!({"item": kept["reference"], "site": "https://example.com"}),
    );
    let allowed = super::super::tests::approved(&fill, &workspace);
    // A use of the vault's one entry, or an edit of it, written as the host writes for the
    // agent's call `call`.
    let write = |editing: bool, call| {
        let agent = &agent;
        async move {
            let opened = workspaces
                .open_vault(&workspace_id.to_string())
                .await
                .unwrap();
            let (entry, mut uses) = opened.entries()[0].clone();
            let change = if editing {
                let mut edited = entry;
                edited.password = "planted".to_owned();
                VaultChange::Keep(edited)
            } else {
                uses.push(UseRecord {
                    at_unix: 7,
                    places: Vec::new(),
                    by: "Assistant".to_owned(),
                    using: "tilo.vault/fill-page".to_owned(),
                    what: String::new(),
                    allowed: None,
                });
                VaultChange::Uses(entry.id, uses)
            };
            workspaces
                .write_vault(&opened, vec![change], Some(agent), Some(call))
                .await
        }
    };

    write(false, &allowed)
        .await
        .expect("the use a fill declares is written down under her yes");
    let refused = write(true, &allowed).await;
    assert!(refused.is_err(), "a fill keeps no item: {refused:?}");

    let opened = workspaces
        .open_vault(&workspace_id.to_string())
        .await
        .unwrap();
    let (entry, uses) = &opened.entries()[0];
    assert_eq!(entry.password, PASSWORD, "the item is as she kept it");
    assert_eq!(uses.len(), 1, "one use: {uses:?}");
    // The use is the agent's, for her, and keeps the call it was made for with her yes.
    let history = vault_history(workspaces, workspace_id).await;
    assert_eq!(history.last(), Some(&(vec![hers], true)), "{history:?}");
    assert_eq!(
        last_allowed(workspaces, workspace_id).await,
        tilo_actions::remote::CallAllowed::asked(&allowed)
    );
    server.shutdown().await;
}

/// A person's yes admits the one call it answered: answering it again admits nothing more, the
/// same use of another item asks her again, and a yes the agent's device sends of its own is
/// none. Nothing the second item keeps is used.
#[tokio::test]
async fn a_yes_admits_the_one_call_it_answered() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let (workspace, workspace_id, service) = governed(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let provider = a_provider(&service, workspace_id, (&ada, &as_ada)).await;
    let first = kept_item(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("key", "https://models.example/v1/chat"),
    )
    .await;
    let second = kept_item(
        &service,
        workspace_id,
        (&ada, &as_ada),
        ("key", "https://models.example/v1/chat"),
    )
    .await;
    let helper = Caller::delegated_agent("assistant", ada.user_public_key, ada.device_public_key)
        .in_workspace(workspace_id.to_string())
        .named("Helper");
    let keeping = |item: &Value| {
        request_of(
            model_providers::target(model_providers::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": item}),
        )
    };

    // She allows the first.
    let wanted = keeping(&first);
    asks(asked(&service, workspace_id, (&ada, &helper), &wanted).await);
    let page = service.device_page(workspace_id, &ada);
    super::waiting::answered_by(
        &page,
        &as_ada,
        (&helper, &wanted),
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await
    .expect("allowed, the key is kept");
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &first).await;
    assert_eq!(uses.as_array().map(Vec::len), Some(1), "{uses}");

    // Answered again, her yes admits nothing more.
    let again = super::waiting::answered_by(
        &page,
        &as_ada,
        (&helper, &wanted),
        tilo_actions::remote::asked::Offer::Approve,
    )
    .await;
    assert!(again.is_err(), "{again:?}");
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &first).await;
    assert_eq!(uses.as_array().map(Vec::len), Some(1), "{uses}");

    // The same use of another item asks her again, and a yes its device sends of its own
    // is none.
    asks(
        ask_at(
            &service,
            workspace_id,
            (&ada, &helper),
            model_providers::target(model_providers::SET_KEY_FROM_ITEM),
            json!({"provider": provider, "key": second}),
        )
        .await,
    );
    let forged = super::super::tests::approved(&keeping(&second), &workspace);
    let sent = asked(&service, workspace_id, (&ada, &helper), &forged).await;
    assert!(sent.is_err(), "{sent:?}");
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &second).await;
    assert_eq!(uses, json!([]), "the second item is used nowhere");
    server.shutdown().await;
}

/// How the call the last change of `workspace_id`'s vault keeps was allowed.
async fn last_allowed(
    workspaces: &Workspaces,
    workspace_id: WorkspaceId,
) -> Option<tilo_actions::remote::CallAllowed> {
    let bytes = workspaces
        .resource_document(
            &workspace_id.to_string(),
            tilo_app_id::apps::PASSWORDS,
            tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
        )
        .await
        .unwrap()
        .expect("the workspace keeps its vault");
    let doc = automerge::Automerge::load(&bytes).unwrap();
    doc.get_changes(&[])
        .into_iter()
        .filter_map(|change| {
            change
                .message()
                .and_then(tilo_actions::ActionContext::from_change_message)
        })
        .last()
        .and_then(|context| context.signed)
        .and_then(|call| call.allowed)
}
