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
use tilo_server_protocol::WorkspaceId;
use tilo_user_identity::DeviceCert;
use tilo_workspace_records::accounts as workspace_accounts;
use tilo_workspace_records::models as model_providers;
use tilo_workspace_records::vault as workspace_vault;

use crate::fixtures::{admit, caller, hosted, settled, signed_from};
use crate::fixtures::{guest_device, hosting_device};
use crate::fixtures::{install_app, revoke_grant, test_host, test_host_holding};
use crate::hosted_use::turn_use_tests::asks;
use tilo_workspace_service::workspaces::actions::ServiceWorkspaceActions;
use tilo_workspace_service::workspaces::fixtures::given;
use tilo_workspace_service::workspaces::workspace_stores::Store;

const PASSWORD: &str = "correct horse battery staple";
const NAME: &str = "ada@example.test";
/// What no site shows as a setup key: it holds digits a seed never does.
const NOT_A_SEED: &str = "correct horse battery staple 0189";
const KEY: &str = "sk-live-ada-own-key";

/// Install Passwords in `workspace`, as a member installs it: what gives it its own records,
/// the vault among them. The vault is the workspace's own record, which no surface of the
/// app's declares.
async fn install_passwords(workspaces: &crate::PersonServices, workspace: &str) {
    install_app(workspaces, workspace, tilo_app_id::apps::PASSWORDS)
        .await
        .unwrap();
}

/// What `workspaces` answers `caller` for `action` of `store` in `workspace_id`.
async fn reach(
    workspaces: &crate::PersonServices,
    (workspace_id, caller, device): (WorkspaceId, &Caller, Option<&DeviceCert>),
    (store, action, effect): (Store, &str, tilo_action_types::ActionKind),
) -> Result<tilo_action_policy::Answer, ActionCallError> {
    workspaces
        .reaching((Some(workspace_id), caller, device), store, action, effect)
        .await
        .map(|reach| reach.answer)
}

/// Who each change of `workspace_id`'s vault names as made for, in order: the people its
/// history entry names, and whether it keeps the call they signed.
async fn vault_history(
    workspaces: &crate::PersonServices,
    workspace_id: WorkspaceId,
) -> Vec<(Vec<String>, bool)> {
    let bytes = workspaces
        .resource_document(
            &workspace_id.to_string(),
            tilo_app_id::apps::PASSWORDS,
            tilo_actions::remote::VAULT_RECORD,
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
pub(crate) async fn ask_at(
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
pub(crate) fn request_of(target: RemoteActionTarget, argument: Value) -> InvokeRequest {
    let argument = match argument {
        Value::Object(map) if !map.is_empty() => InvokeArgument::Json(map),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        asks: false,
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
pub(crate) async fn asked(
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
    crate::waiting::at_once(
        &crate::fixtures::workspaces_served(service),
        request,
        receipt,
    )
}

/// `action` of the vault, asked as [`ask_at`] asks.
pub(crate) async fn ask(
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
        workspace_vault::target(action),
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
    crate::waiting::answered_by(
        &service.device_page(workspace_id, device),
        &crate::fixtures::caller(device),
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
pub(crate) fn files_holding(dir: &std::path::Path, needle: &[u8]) -> Vec<std::path::PathBuf> {
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let (as_ada, as_bo) = (caller(&ada), caller(&bo));

    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://www.linkedin.com/login?session=1"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].as_str().expect("a reference").to_owned();
    assert!(
        workspace_vault::ItemRef::parse(&reference).is_some(),
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
            workspace_vault::LIST,
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
    let (elsewhere, elsewhere_id, _) = hosted(workspaces).await;
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
            workspace_vault::LIST,
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
            workspace_vault::REMOVE,
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
        workspace_vault::LIST,
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
                tilo_actions::remote::VAULT_RECORD,
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
        workspace_vault::REMOVE,
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
#[cfg(feature = "agents")]
#[tokio::test]
async fn an_items_title_note_fields_and_tags_are_the_vaults_and_list_without_secrets() {
    const NOTE: &str = "made-up-recovery-words-4471";
    const PIN: &str = "made-up-pin-7351";
    const STRONG: &str = "Made-up-Strong-Pass-0451";
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let (lines, logging) = crate::hosted_use::web_call_tests::logged();

    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
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
        workspace_vault::ADD,
        json!({"kind": "login", "belongsTo": ["https://shop.example"], "secret": STRONG}),
    )
    .await
    .expect("Ada keeps the same password for a shop");
    let drawn = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
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
            workspace_vault::ADD,
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
        workspace_vault::LIST,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let reference = kept["reference"].clone();
    let banked = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        json!({"kind": "note", "title": "Bank", "secret": PASSWORD}),
    )
    .await
    .expect("Ada keeps a note");

    // Her agent, over the connection her device handed it, reads the
    // items, but is shown and copied none of their secrets.
    let agent = as_ada.clone().delegated_to("assistant");
    let listed = ask(
        &service,
        workspace_id,
        (&ada, &agent),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .expect("her agent reads the items");
    // One vault: her note, which belongs nowhere, is an item beside her login.
    let items = listed["items"].as_array().cloned().unwrap_or_default();
    assert_eq!(items.len(), 2, "{listed}");
    assert!(items.contains(&kept), "{listed}");
    assert!(items.contains(&banked), "{listed}");
    for showing in [workspace_vault::REVEAL, workspace_vault::COPY] {
        let refused = ask(
            &service,
            workspace_id,
            (&ada, &agent),
            showing,
            json!({"item": &reference}),
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{showing}: {refused:?}"
        );
    }
    // Nor is a Shortcut's unattended run, whether she or her agent saved it: nobody is there to
    // be shown the secret.
    for (saved_by, run) in [
        ("her", as_ada.clone().running_shortcut("reminder")),
        ("her agent", agent.clone().running_shortcut("reminder")),
    ] {
        for showing in [workspace_vault::REVEAL, workspace_vault::COPY] {
            let refused = ask(
                &service,
                workspace_id,
                (&ada, &run),
                showing,
                json!({"item": &reference}),
            )
            .await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "a shortcut {saved_by} saved, {showing}: {refused:?}"
            );
        }
    }

    // Every change waits for its person: with nobody to ask, their answer
    // is still wanted and nothing changed.
    for (action, argument) in [
        (workspace_vault::ADD, a_login("https://attacker.example")),
        (workspace_vault::REMOVE, json!({"item": &reference})),
    ] {
        let refused = ask(&service, workspace_id, (&ada, &agent), action, argument).await;
        assert!(
            matches!(refused, Err(ActionCallError::ConfirmationRequired(_))),
            "{action}: {refused:?}"
        );
    }
    let unchanged = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::LIST,
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
            workspace_vault::target(workspace_vault::ADD),
            a_login("https://example.test"),
        ),
    )
    .await
    .expect("her agent keeps what she allowed");
    let now = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(now["items"].as_array().map(Vec::len), Some(3));

    // Nobody else reaches it, however it names itself.
    let person = hex::encode(ada.user_public_key);
    let not_her: [(&str, &DeviceCert, Caller); 5] = [
        ("a bot", &ada, Caller::local_bot("a-bot")),
        (
            "an app acting for her",
            &ada,
            Caller::local_app("so.tilo.chat").acting_for(person.clone()),
        ),
        (
            "her Passwords on another person's device",
            &bo,
            caller(&ada).through_app(tilo_app_id::apps::PASSWORDS),
        ),
        (
            "an agent the server admitted itself, acting for her",
            &ada,
            Caller::local_agent("assistant", person),
        ),
        ("her key on another person's device", &bo, caller(&ada)),
    ];
    for (who, device, asking) in &not_her {
        for (action, argument) in [
            (workspace_vault::LIST, json!({})),
            (workspace_vault::ADD, a_login("https://attacker.example")),
            (workspace_vault::REMOVE, json!({"item": &reference})),
            (workspace_vault::REVEAL, json!({"item": &reference})),
            (workspace_vault::COPY, json!({"item": &reference})),
            (workspace_vault::USES, json!({"item": &reference})),
        ] {
            let refused = ask(&service, workspace_id, (device, asking), action, argument).await;
            assert!(
                matches!(refused, Err(ActionCallError::UnknownAction)),
                "{who}, {action}: {refused:?}"
            );
        }
    }

    // Another app on her own device holds no grant to the vault: what it calls is asked of
    // her first, and nothing is done until she answers.
    let notes = caller(&ada).through_app("so.tilo.notes");
    for (action, argument) in [
        (workspace_vault::LIST, json!({})),
        (workspace_vault::ADD, a_login("https://attacker.example")),
        (workspace_vault::REMOVE, json!({"item": &reference})),
        (workspace_vault::REVEAL, json!({"item": &reference})),
        (workspace_vault::COPY, json!({"item": &reference})),
    ] {
        let asked = ask(&service, workspace_id, (&ada, &notes), action, argument).await;
        assert!(
            matches!(asked, Err(ActionCallError::ConfirmationRequired(_))),
            "another app, {action}: {asked:?}"
        );
    }

    // Nothing of the workspace's moved.
    let still = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::LIST,
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
    use tilo_action_policy::{Answer, Grant, Grantee, TreePath};
    use tilo_action_types::KindSet;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
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
    given(
        workspaces,
        &workspace,
        &owner,
        vec![
            everything(
                "test:agent-everywhere",
                Grantee::agent("assistant", hex::encode(stranger.user_public_key).clone()),
            ),
            everything(
                "test:stranger-everywhere",
                Grantee::person(hex::encode(stranger.user_public_key)),
            ),
        ],
    )
    .await;

    for (action, argument) in [
        (workspace_vault::ADD, a_login("https://attacker.example")),
        (workspace_vault::REMOVE, json!({"item": kept["reference"]})),
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
            matches!(by_agent, Err(ActionCallError::ConfirmationRequired(_))),
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
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(still["items"], json!([kept]), "{still}");
    server.shutdown().await;
}

/// A clipboard that keeps what it was handed, standing in for the session's.
#[derive(Default)]
struct Clipboard(std::sync::Mutex<Vec<String>>);

impl crate::SecretClipboard for Clipboard {
    fn copy_secret(&self, secret: &str) -> bool {
        self.0.lock().unwrap().push(secret.to_owned());
        true
    }
}

/// Every member lists the workspace's items, a note and a Wi-Fi password among them, and each
/// is shown or has copied one secret of an item at a time, as themselves or through Passwords on
/// their own device: each showing is a use, naming Passwords where it went through it. No
/// answer but a reveal's holds the secret, and a copy's lands on the device's clipboard alone.
/// No other app reaches any of it, and Passwords never hands an item to anything else. A member
/// of another workspace reads none of it.
#[tokio::test]
async fn passwords_lists_shows_and_copies_the_workspaces_items_for_its_person_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let clipboard = Arc::new(Clipboard::default());
    workspaces.copy_secrets_to(clipboard.clone());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let bo = admit(workspaces, &workspace, &owner, 2).await;
    install_passwords(workspaces, &workspace).await;
    let passwords = caller(&ada).through_app(tilo_app_id::apps::PASSWORDS);
    let hers = (&ada, &passwords);

    // Passwords finds the vault's own calls by their address, and no other app finds them.
    let offered = |asking: Caller| {
        let provider = service.in_workspace_as(workspace_id, &ada);
        async move {
            let page = settled(provider.query(
                &asking,
                tilo_actions::remote::CatalogueQuery {
                    provider: Some(workspace_vault::PROVIDER.to_owned()),
                    ..tilo_actions::remote::CatalogueQuery::default()
                },
            ))
            .await
            .unwrap();
            let mut verbs: Vec<String> = page
                .rows
                .into_iter()
                .filter_map(|row| row.action)
                .filter(|action| action.address.resource == workspace_vault::RESOURCE)
                .map(|action| action.address.action)
                .collect();
            verbs.sort();
            verbs
        }
    };
    assert_eq!(
        offered(passwords.clone()).await,
        [
            "add",
            "copy",
            "list",
            "remove",
            "reveal",
            "take-back",
            "uses"
        ],
        "Passwords is offered the vault's own calls, never a release"
    );
    assert!(
        offered(caller(&ada).through_app("so.tilo.notes"))
            .await
            .is_empty()
    );

    let wifi = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::ADD,
        json!({
            "kind": "wifi",
            "title": "Home",
            "secret": PASSWORD,
            "notes": "router in the hall",
            "fields": [{"label": "network", "value": "home-5g"}],
            "tags": ["house"],
        }),
    )
    .await
    .expect("Passwords keeps a Wi-Fi password for Ada");
    let reference = wifi["reference"].clone();
    assert_eq!(wifi["kind"], "wifi");
    assert_eq!(wifi["fields"], json!(["network"]));
    assert!(!wifi.to_string().contains(PASSWORD), "{wifi}");

    // Every member lists it, and nothing of its secrets.
    let listed = ask(
        &service,
        workspace_id,
        (&bo, &caller(&bo)),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(listed["items"], json!([wifi]), "{listed}");

    // Shown and copied, one part at a time.
    for (part, said) in [
        (None, PASSWORD),
        (Some("notes"), "router in the hall"),
        (Some("field:network"), "home-5g"),
    ] {
        let mut showing = json!({"item": &reference});
        if let Some(part) = part {
            showing["part"] = json!(part);
        }
        let shown = ask(
            &service,
            workspace_id,
            hers,
            workspace_vault::REVEAL,
            showing.clone(),
        )
        .await
        .unwrap_or_else(|error| panic!("{part:?}: {error:?}"));
        assert_eq!(shown, json!({"value": said}));
        let copied = ask(&service, workspace_id, hers, workspace_vault::COPY, showing)
            .await
            .unwrap();
        assert!(!copied.to_string().contains(said), "{copied}");
        assert_eq!(
            clipboard.0.lock().unwrap().last().map(String::as_str),
            Some(said)
        );
    }
    // Bo, as himself, is shown it too: it is the workspace's.
    let his = ask(
        &service,
        workspace_id,
        (&bo, &caller(&bo)),
        workspace_vault::REVEAL,
        json!({"item": &reference}),
    )
    .await
    .unwrap();
    assert_eq!(his["value"], PASSWORD);
    let uses = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::USES,
        json!({"item": &reference}),
    )
    .await
    .unwrap();
    let uses = uses["uses"].as_array().cloned().unwrap();
    assert_eq!(uses.len(), 7, "each showing and copy is a use: {uses:?}");
    assert_eq!(
        uses.iter().filter(|used| used["by"] == "Passwords").count(),
        6,
        "{uses:?}"
    );
    assert!(uses.iter().any(|used| used["using"] == "copy"), "{uses:?}");
    assert!(!json!(uses).to_string().contains(PASSWORD));
    // A part the item does not keep is none to show, and says nothing of what it holds.
    let none = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::REVEAL,
        json!({"item": &reference, "part": "field:PIN"}),
    )
    .await;
    assert!(
        matches!(none, Err(ActionCallError::Refused { .. })),
        "{none:?}"
    );

    // Ada renames it and drops its note's tag: the same item, its secrets and uses as they were.
    let changed = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::ADD,
        json!({
            "item": &reference,
            "kind": "wifi",
            "title": "Home upstairs",
            "fields": [{"label": "network", "value": ""}],
        }),
    )
    .await
    .expect("Passwords changes the item in place");
    assert_eq!(changed["reference"], reference);
    assert_eq!(changed["title"], "Home upstairs");
    assert_eq!(changed["fields"], json!(["network"]));
    assert!(changed.get("tags").is_none(), "{changed}");
    let still = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::REVEAL,
        json!({"item": &reference, "part": "field:network"}),
    )
    .await
    .unwrap();
    assert_eq!(still["value"], "home-5g");

    // Passwords hands an item to nothing else, and every other app asks its person first.
    let released = ask_at(
        &service,
        workspace_id,
        hers,
        workspace_vault::ItemRef::parse(reference.as_str().unwrap())
            .unwrap()
            .target(workspace_vault::RELEASE),
        json!({}),
    )
    .await;
    assert!(released.is_err(), "{released:?}");
    for app in ["so.tilo.notes", "so.tilo.browser", "so.tilo.chat"] {
        let other = caller(&ada).through_app(app);
        for (action, argument) in [
            (workspace_vault::LIST, json!({})),
            (workspace_vault::ADD, a_login("https://attacker.example")),
            (workspace_vault::REVEAL, json!({"item": &reference})),
            (workspace_vault::COPY, json!({"item": &reference})),
            (workspace_vault::REMOVE, json!({"item": &reference})),
        ] {
            let asked = ask(&service, workspace_id, (&ada, &other), action, argument).await;
            assert!(
                matches!(asked, Err(ActionCallError::ConfirmationRequired(_))),
                "{app}, {action}: {asked:?}"
            );
        }
    }

    // A member of another workspace reads none of it, there or here.
    let (elsewhere, elsewhere_id, _) = hosted(workspaces).await;
    let cy = admit(workspaces, &elsewhere, &owner, 3).await;
    for (action, argument) in [
        (workspace_vault::LIST, json!({})),
        (workspace_vault::REVEAL, json!({"item": &reference})),
    ] {
        let refused = ask(
            &service,
            workspace_id,
            (&cy, &caller(&cy)),
            action,
            argument,
        )
        .await;
        assert!(
            matches!(refused, Err(ActionCallError::UnknownAction)),
            "{action}: {refused:?}"
        );
    }
    let theirs = ask(
        &service,
        elsewhere_id,
        (&cy, &caller(&cy)),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(theirs["items"], json!([]), "{theirs}");

    // Passwords removes it, and it is gone for everyone.
    let left = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::REMOVE,
        json!({"item": &reference}),
    )
    .await
    .unwrap();
    assert_eq!(left["items"], json!([]));
    server.shutdown().await;
}

/// An item that is not one to keep is refused before anything is kept, and
/// the refusal never repeats what was typed: a secret put where a place or a
/// kind belongs is not echoed back.
#[tokio::test]
async fn an_item_that_is_not_one_is_refused_without_repeating_it() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);

    for unusable in [
        json!({"kind": "login", "belongsTo": [PASSWORD], "secret": PASSWORD}),
        json!({"kind": PASSWORD, "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": PASSWORD, "secret": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": [PASSWORD]}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "password": PASSWORD}),
        json!({"kind": "login", "belongsTo": ["https://example.com"], "secret": ""}),
        json!({"kind": "ssh-key", "belongsTo": ["https://example.com"], "secret": PASSWORD}),
        json!({"kind": "code-generator", "belongsTo": ["https://example.com"], "secret": NOT_A_SEED}),
    ] {
        let refused = ask(
            &service,
            workspace_id,
            (&ada, &as_ada),
            workspace_vault::ADD,
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
        workspace_vault::REMOVE,
        json!({"item": PASSWORD}),
    )
    .await
    .expect_err("what is no reference names nothing");
    assert!(!format!("{refused} {refused:?}").contains(PASSWORD));

    let hers = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    assert_eq!(hers["items"], json!([]));
    assert!(files_holding(dir.path(), PASSWORD.as_bytes()).is_empty());
    server.shutdown().await;
}

/// A provider of whoever asks, at `https://models.example/v1`, by its id.
pub(crate) async fn a_provider(
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
pub(crate) async fn kept_item(
    service: &ServiceWorkspaceActions,
    workspace_id: WorkspaceId,
    who: (&DeviceCert, &Caller),
    (kind, place): (&str, &str),
) -> Value {
    let kept = ask(
        service,
        workspace_id,
        who,
        workspace_vault::ADD,
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
        workspace_vault::USES,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
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
        matches!(unasked, Err(ActionCallError::ConfirmationRequired(_))),
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
        workspace_vault::LIST,
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
        workspace_vault::USES,
        json!({"item": item}),
    )
    .await
    .expect("the agent reads what its item remembers");
    assert_eq!(read["uses"], uses);
    let taken = ask(
        &service,
        workspace_id,
        (&ada, &helper),
        workspace_vault::TAKE_BACK,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let (elsewhere, elsewhere_id, _) = hosted(workspaces).await;
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
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
            Caller::local_agent("assistant", person),
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
        matches!(typed, Err(ActionCallError::ConfirmationRequired(_))),
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let hers = (&ada, &as_ada);
    let kept = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::ADD,
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
    let with_item = workspace_accounts::target(workspace_accounts::ADD_MAIL_WITH_ITEM);

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
        workspace_accounts::target(workspace_accounts::ADD_MAIL),
        json!({
            "address": NAME,
            "incomingHost": "imap.example.test",
            "outgoingHost": "smtp.example.test",
            "password": PASSWORD,
        }),
    )
    .await;
    assert!(
        matches!(typed, Err(ActionCallError::ConfirmationRequired(_))),
        "{typed:?}"
    );

    let accounts = ask_at(
        &service,
        workspace_id,
        hers,
        workspace_accounts::target(workspace_accounts::LIST),
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
fn provider_named(root: &std::path::Path, provider: &str) -> Option<workspace_vault::ItemRef> {
    crate::person_settings::provider_config(root)
        .profiles
        .iter()
        .find(|profile| profile.id == provider)
        .and_then(crate::person_settings::key_reference)
}

/// The entry of the workspace's vault the account `account` of the workspace `workspace` names
/// as its sign-in.
async fn account_named(
    workspaces: &crate::PersonServices,
    workspace: &str,
    account: &str,
) -> Option<workspace_vault::ItemRef> {
    workspaces
        .workspace_accounts(workspace)
        .await
        .unwrap()
        .account(account)
        .and_then(|kept| {
            accounts::email::password_item(&kept.config).and_then(workspace_vault::ItemRef::parse)
        })
}

/// What the entry `named` of the workspace's vault holds.
async fn entry_of(
    workspaces: &crate::PersonServices,
    workspace_id: WorkspaceId,
    named: &workspace_vault::ItemRef,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
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

    let listed = ask(
        &service,
        workspace_id,
        hers,
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .unwrap();
    // The item the host kept, and the account's sign-in, an entry of the vault like any other.
    let items = listed["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{listed}");
    let passwords_entry = json!(format!("vault.item:{}", entry.id.0));
    assert!(
        items
            .iter()
            .any(|item| item["reference"] == passwords_entry),
        "the Passwords entry is in the vault: {listed}"
    );
    let carried = items
        .iter()
        .find(|item| item["reference"] == json!(format!("vault.item:{id}")))
        .unwrap_or_else(|| panic!("the kept item is listed: {listed}"));
    assert_eq!(carried["belongsTo"], json!(["https://example.test"]));
    let sign_in = items
        .iter()
        .find(|item| {
            item["reference"] != carried["reference"] && item["reference"] != passwords_entry
        })
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
        workspace_vault::USES,
        json!({"item": format!("vault.item:{id}")}),
    )
    .await
    .unwrap();
    assert_eq!(uses["uses"].as_array().map(Vec::len), Some(1), "{uses}");

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
    let (key, owner) = hosting_device();
    // Its key service keeps the workspace's document keys across a restart, as a server's does:
    // the workspace's accounts are sealed for them.
    let keys = Arc::new(crate::fixtures::TestGroupKeys::default());
    let server = test_host_holding(dir.path(), &key, &owner, Arc::clone(&keys)).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (_, workspace_id, _) = hosted(workspaces).await;
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
    crate::person_settings::save_provider_config(
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
    let workspaces = &crate::PersonServices::of(server.workspaces());
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
    let signing_in = crate::person_settings::account_secret_for_test(workspaces, &workspace, NAME)
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    // Ada's workspace, which the host founded for her and serves; her device makes the host's
    // own person an administrator there.
    let (ada_key, ada) = guest_device();
    let workspace = workspaces
        .create_for("Ada's", None, ada.user_public_key)
        .await
        .unwrap()
        .workspace;
    workspaces
        .enter(&workspace, (&ada, &ada_key))
        .await
        .unwrap();
    tilo_workspace_service::workspaces::fixtures::remember_admitted(&ada, &ada_key);
    tilo_workspace_service::workspaces::fixtures::made_as(
        workspaces,
        &workspace,
        &ada,
        (
            owner.user_public_key,
            tilo_peer_state::workspace_membership::MemberPlace::Administrator,
        ),
    )
    .await
    .unwrap();
    // The change her device signed lets the host read Passwords: it holds the vault's keys, which
    // a device holding the keys seals them for it as for any device the workspace newly lets read them.
    let vault_keys_with = async || {
        let held = workspaces.workspace(&workspace).await.unwrap();
        let held = held.lock().await;
        held.peer()
            .document_key_holders(
                tilo_app_id::apps::PASSWORDS,
                tilo_actions::remote::VAULT_RECORD,
                tilo_clock::wall(),
            )
            .unwrap()
    };
    assert!(vault_keys_with().await.contains(&owner.device_public_key));
    let signing_in = async || {
        crate::person_settings::account_secret_for_test(workspaces, &workspace, NAME).await
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
                document: tilo_actions::remote::VAULT_RECORD.into(),
                recipient: format!("member:{}", hex::encode(owner.user_public_key)),
                access: "no-access".into(),
            },
        )
        .await;
    assert!(matches!(reply, WorkspacesReply::Workspace(_)), "{reply:?}");

    let refused = signing_in().await;
    assert!(refused.is_err(), "the host still signs in: {refused:?}");
    assert!(
        !vault_keys_with().await.contains(&owner.device_public_key),
        "the host still holds the vault's keys"
    );
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (_workspace, workspace_id, service) = hosted(workspaces).await;
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
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
    let item: workspace_vault::ItemRef = serde_json::from_value(kept["reference"].clone())
        .expect("the item is named by its reference");

    let said = ask_at(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ItemRef::target(&item, workspace_vault::CODE),
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &caller(&ada)),
        workspace_vault::ADD,
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let hers = hex::encode(ada.user_public_key);
    let agent = tilo_member_types::Acting::agent("assistant", hers.clone())
        .acting_for(tilo_member_types::Acting::person(hers.clone()));
    let fill = request_of(
        workspace_vault::target(workspace_vault::FILL_PAGE),
        json!({"item": kept["reference"], "site": "https://example.com"}),
    );
    let allowed = crate::fixtures::approved(&fill, &workspace);
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
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
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
    crate::waiting::answered_by(
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
    let again = crate::waiting::answered_by(
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
    let forged = crate::fixtures::approved(&keeping(&second), &workspace);
    let sent = asked(&service, workspace_id, (&ada, &helper), &forged).await;
    assert!(sent.is_err(), "{sent:?}");
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &second).await;
    assert_eq!(uses, json!([]), "the second item is used nowhere");
    server.shutdown().await;
}

/// How the call the last change of `workspace_id`'s vault keeps was allowed.
async fn last_allowed(
    workspaces: &crate::PersonServices,
    workspace_id: WorkspaceId,
) -> Option<tilo_actions::remote::CallAllowed> {
    let bytes = workspaces
        .resource_document(
            &workspace_id.to_string(),
            tilo_app_id::apps::PASSWORDS,
            tilo_actions::remote::VAULT_RECORD,
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
        .next_back()
        .and_then(|context| context.signed)
        .and_then(|call| call.allowed)
}

/// An app reaches the workspace's stores by the grants naming it, never by its name. An app that
/// calls itself Mail reads no sign-ins until its signed manifest declares it signs in, and then
/// only those; Passwords reaches the vault once installing it gave it its own records, and
/// asks its person before that and again once that grant is taken back.
#[tokio::test]
async fn an_app_reaches_a_store_by_its_grants_never_by_its_name() {
    use tilo_action_policy::Answer;
    use tilo_action_types::ActionKind;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, _) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let mail = tilo_app_id::apps::MAIL;
    let sign_ins = (Store::MAIL, workspace_accounts::SIGN_INS, ActionKind::Read);

    // Named Mail, with no grant: nothing.
    let named = Caller::launched(mail).in_workspace(&workspace);
    assert!(
        matches!(
            reach(workspaces, (workspace_id, &named, None), sign_ins).await,
            Err(ActionCallError::NotPermitted)
        ),
        "a name is no grant"
    );

    // With the grant its signed manifest declares: the sign-ins, and nothing else.
    let declared = named.clone().granting(vec![tilo_action_policy::Grant::new(
        workspace_accounts::sign_in_grant_id(mail, &workspace),
        tilo_action_policy::Grantee::app(mail),
        workspace_accounts::sign_ins_node(&workspace),
        tilo_action_types::KindSet::READ,
        Answer::Allow,
        tilo_action_policy::Grantee::person(hex::encode(ada.user_public_key)),
    )]);
    assert_eq!(
        reach(workspaces, (workspace_id, &declared, None), sign_ins).await,
        Ok(Answer::Allow)
    );
    for (store, action, effect) in [
        (
            Store::MAIL,
            workspace_accounts::SET_SERVICES,
            ActionKind::Undoable,
        ),
        (Store::VAULT, workspace_vault::LIST, ActionKind::Read),
    ] {
        assert!(
            matches!(
                reach(
                    workspaces,
                    (workspace_id, &declared, None),
                    (store, action, effect)
                )
                .await,
                Err(ActionCallError::NotPermitted)
            ),
            "{action}"
        );
    }

    // Passwords, named but not installed: alone it reaches nothing, and through its person
    // the vault asks them first.
    let passwords = tilo_app_id::apps::PASSWORDS;
    let list = (Store::VAULT, workspace_vault::LIST, ActionKind::Read);
    let alone = Caller::launched(passwords).in_workspace(&workspace);
    let through_ada = caller(&ada).through_app(passwords);
    assert!(matches!(
        reach(workspaces, (workspace_id, &alone, None), list).await,
        Err(ActionCallError::NotPermitted)
    ));
    assert_eq!(
        reach(workspaces, (workspace_id, &through_ada, Some(&ada)), list).await,
        Ok(Answer::Ask)
    );

    // Installed, the grant to its own records reaches the vault.
    install_passwords(workspaces, &workspace).await;
    assert_eq!(
        reach(workspaces, (workspace_id, &through_ada, Some(&ada)), list).await,
        Ok(Answer::Allow)
    );
    assert_eq!(
        reach(workspaces, (workspace_id, &alone, None), list).await,
        Ok(Answer::Allow)
    );

    // Taken back, it asks again.
    revoke_grant(
        workspaces,
        &workspace,
        tilo_action_policy::roles::app_records_grant_id(passwords),
    )
    .await
    .unwrap();
    assert_eq!(
        reach(workspaces, (workspace_id, &through_ada, Some(&ada)), list).await,
        Ok(Answer::Ask)
    );
    server.shutdown().await;
}

/// The person's own windows and palette ask the workspace's tree for its model providers
/// (`workspace/models`), and both actions a provider answers apart from the rest are there: a
/// key the vault keeps, named by its item, is kept as the use of the vault it is, and linking
/// this computer's models reaches the provider, which says why a host keeping the workspace for
/// a page links none.
#[tokio::test]
async fn the_person_keeps_a_key_from_the_vault_and_links_this_computer_through_the_tree() {
    use tilo_actions::remote::{
        WORKSPACE_GOVERNANCE_PROVIDER, WORKSPACE_MODELS, workspace_governance_child,
    };

    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
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
    let in_tree = |action: &str| RemoteActionTarget {
        provider: WORKSPACE_GOVERNANCE_PROVIDER.to_owned(),
        address: tilo_actions::ActionAddress::new(
            workspace_governance_child(&workspace, Some(WORKSPACE_MODELS)),
            action,
        ),
    };

    let view = ask_at(
        &service,
        workspace_id,
        (&ada, &as_ada),
        in_tree(model_providers::SET_KEY_FROM_ITEM),
        json!({"provider": provider, "key": item}),
    )
    .await
    .expect("Ada keeps her provider's key from the item her vault keeps");
    assert_eq!(view["configured"], json!(true), "{view}");
    assert!(!view.to_string().contains(KEY), "{view}");
    let own = workspaces.workspace_root(&workspace_id.to_string());
    let named = provider_named(&own, &provider).expect("the provider names its key");
    assert_eq!(json!(named.to_string()), item);
    let uses = uses_of(&service, workspace_id, (&ada, &as_ada), &item).await;
    assert_eq!(uses[0]["using"], "tilo.models/set-key-from-item", "{uses}");

    let linked = ask_at(
        &service,
        workspace_id,
        (&ada, &as_ada),
        in_tree(model_providers::LINK_THIS_COMPUTER),
        json!({}),
    )
    .await;
    match linked {
        Err(ActionCallError::UnknownAction) => panic!("linking this computer is not in the tree"),
        Err(refused) => assert!(
            format!("{refused:?}").contains("Only a computer of yours links its own models"),
            "{refused:?}"
        ),
        Ok(answer) => panic!("a host keeping the workspace linked its own models: {answer}"),
    }
    server.shutdown().await;
}

/// A Shortcut her agent saved runs through that agent, for her: what the workspace keeps the agent
/// from, its run is kept from too, though she reaches it herself and so does a Shortcut she saved.
#[tokio::test]
async fn a_run_of_a_shortcut_her_agent_saved_is_kept_from_what_the_agent_is() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let kept_from_vault = tilo_action_policy::Grant::new(
        "test:assistant-kept-from-vault",
        tilo_action_policy::Grantee::agent("assistant", hex::encode(ada.user_public_key)),
        tilo_action_policy::TreePath::workspace(&workspace)
            .app(tilo_app_id::apps::PASSWORDS)
            .kind("passwords.workspace-vault"),
        tilo_action_types::KindSet::ALL,
        tilo_action_policy::Answer::Deny,
        tilo_action_policy::Grantee::person(hex::encode(owner.user_public_key)),
    );
    given(workspaces, &workspace, &owner, vec![kept_from_vault]).await;

    for (who, asking) in [
        ("she", as_ada.clone()),
        (
            "a Shortcut she saved",
            as_ada.clone().running_shortcut("reminder"),
        ),
    ] {
        let listed = ask(
            &service,
            workspace_id,
            (&ada, &asking),
            workspace_vault::LIST,
            json!({}),
        )
        .await
        .unwrap_or_else(|refused| panic!("{who} reads the vault: {refused:?}"));
        assert!(
            listed["items"]
                .as_array()
                .is_some_and(|items| items.contains(&kept)),
            "{who}: {listed}"
        );
    }
    let run = as_ada
        .clone()
        .delegated_to("assistant")
        .running_shortcut("reminder");
    for (action, argument) in [
        (workspace_vault::LIST, json!({})),
        (workspace_vault::REVEAL, json!({"item": &kept["reference"]})),
    ] {
        let refused = ask(&service, workspace_id, (&ada, &run), action, argument).await;
        assert!(
            matches!(
                refused,
                Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)
            ),
            "a run of the Shortcut her agent saved, {action}: {refused:?}"
        );
    }
    server.shutdown().await;
}

/// A Shortcut's run reaches the vault only as far as the rules she saved with it say, whichever
/// call of the run asks: rules for another app keep it out, though she reads the vault herself.
#[tokio::test]
async fn a_shortcuts_run_reads_the_vault_only_within_its_saved_rules() {
    use tilo_action_policy::{Answer, Level, Rule, TreePath};
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    let kept = ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let rules_for = |app: &str| {
        vec![Rule::new(
            TreePath::root().any(Level::Workspace).app(app),
            tilo_action_types::KindSet::ALL,
            Answer::Allow,
        )]
    };
    let run = |app: &str| {
        as_ada
            .clone()
            .running_shortcut("reminder")
            .within_shortcut(rules_for(app))
    };

    let listed = ask(
        &service,
        workspace_id,
        (&ada, &run(tilo_app_id::apps::PASSWORDS)),
        workspace_vault::LIST,
        json!({}),
    )
    .await
    .expect("a run whose rules name the vault reads it");
    assert!(
        listed["items"]
            .as_array()
            .is_some_and(|items| items.contains(&kept)),
        "{listed}"
    );
    let elsewhere = run("so.tilo.notes");
    for (action, argument) in [
        (workspace_vault::LIST, json!({})),
        (workspace_vault::REVEAL, json!({"item": &kept["reference"]})),
    ] {
        let refused = ask(&service, workspace_id, (&ada, &elsewhere), action, argument).await;
        assert!(
            matches!(
                refused,
                Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)
            ),
            "a run whose rules name only another app, {action}: {refused:?}"
        );
    }
    server.shutdown().await;
}

/// A Shortcut's call kept while it waits for her yes goes on within the rules she saved with it:
/// restored, a run whose rules name another app is kept from the vault, and one kept before its
/// rules were kept with it is held to none, so it reaches nothing.
#[tokio::test]
async fn a_restored_shortcut_run_keeps_its_saved_rules() {
    use tilo_action_policy::{Answer, Level, Rule, TreePath};
    use tilo_actions::remote::KeptCaller;
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = hosting_device();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = &crate::PersonServices::of(server.workspaces());
    let (workspace, workspace_id, service) = hosted(workspaces).await;
    let ada = admit(workspaces, &workspace, &owner, 1).await;
    let as_ada = caller(&ada);
    ask(
        &service,
        workspace_id,
        (&ada, &as_ada),
        workspace_vault::ADD,
        a_login("https://example.com"),
    )
    .await
    .expect("Ada keeps a login");
    let kept = |app: &str| {
        serde_json::to_value(
            as_ada
                .clone()
                .running_shortcut("reminder")
                .within_shortcut(vec![Rule::new(
                    TreePath::root().any(Level::Workspace).app(app),
                    tilo_action_types::KindSet::ALL,
                    Answer::Allow,
                )])
                .kept(),
        )
        .unwrap()
    };
    let restored = |record: Value| {
        serde_json::from_value::<KeptCaller>(record)
            .expect("a kept caller decodes")
            .restored()
    };
    let list = |asking: tilo_actions::remote::Caller| {
        let service = &service;
        let ada = &ada;
        async move {
            ask(
                service,
                workspace_id,
                (ada, &asking),
                workspace_vault::LIST,
                json!({}),
            )
            .await
        }
    };

    list(restored(kept(tilo_app_id::apps::PASSWORDS)))
        .await
        .expect("a restored run whose rules name the vault reads it");
    let refused = |who: &str, refused: Result<Value, ActionCallError>| {
        assert!(
            matches!(
                refused,
                Err(ActionCallError::NotPermitted | ActionCallError::UnknownAction)
            ),
            "a restored run, {who}: {refused:?}"
        );
    };
    refused(
        "rules naming another app",
        list(restored(kept("so.tilo.notes"))).await,
    );
    let mut from_before = kept(tilo_app_id::apps::PASSWORDS);
    from_before
        .as_object_mut()
        .unwrap()
        .remove("shortcut_grant")
        .expect("a run's rules are kept with it");
    refused(
        "a record kept with no rules",
        list(restored(from_before)).await,
    );
    server.shutdown().await;
}
