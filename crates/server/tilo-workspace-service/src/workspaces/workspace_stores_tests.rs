//! What a host kept per person before its workspaces kept it moves into the
//! workspaces its person owns as the host opens: one set for the whole
//! workspace, whose members share it, and nothing for a workspace its person
//! is only a member of. What is the person's alone stays where it is, and a
//! person who owns no workspace here keeps their store until one is theirs.

#![expect(
    clippy::significant_drop_tightening,
    reason = "the host a test binds is kept until the test ends; dropping it earlier closes what it drives"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, not a hazard, and a host's start is told whole"
)]

use serde_json::json;
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionProvider as _, Caller, InvokeArgument, InvokeRequest, Pending, ReceiptStatus,
};
use tilo_peer_state::join::Role;
use tilo_server_protocol::workspaces::JoinReply;
use tilo_server_session::workspaces::{
    CREATE_OWN_WORKSPACE, CreatedWorkspace, HOST_ACTIONS_PROVIDER, HOST_WORKSPACES_RESOURCE,
};
use tilo_user_identity::DeviceCert;

use super::actions::ServiceWorkspaceActions;
use super::fixtures::{governor, signed_pre_admission, test_server_keeping_keys_apart};
use super::mail_watch::testing::keep;
use super::person_settings::{provider_config, save_provider_config};
use super::tests::guest_device;
use super::{IssueInvitationRequest, OwnWorkspaceOffer, PreAdmitRequest, Workspaces};

const ACCOUNT: &str = "ada@example.test";
const PASSWORD: &str = "adas-password";
/// What the vault kept under a name only a test gives it.
const SEALED: &str = "the secret only her item holds";
const SECRET: &str = "the entry only she kept";

/// A request the host's own person asks its host, for one of its operations.
fn own_request(name: &str) -> InvokeRequest {
    let mut argument = serde_json::Map::new();
    argument.insert("name".into(), json!(name));
    InvokeRequest {
        invocation: InvocationId::new(),
        target: tilo_actions::remote::RemoteActionTarget {
            provider: HOST_ACTIONS_PROVIDER.into(),
            address: tilo_actions::ActionAddress::new(
                HOST_WORKSPACES_RESOURCE,
                CREATE_OWN_WORKSPACE,
            ),
        },
        argument: InvokeArgument::Json(argument),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
    }
}

/// What a pending answer comes to, on this runtime.
async fn settled<T: Send + 'static>(pending: Pending<T>) -> T {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    pending.then(move |value| {
        let _ = sender.send(value);
    });
    tilo_test_env::within("the host's answer", receiver)
        .await
        .unwrap()
        .expect("answered")
}

/// A workspace of `device`'s own, made through the host's own operation.
async fn create_own(
    host: &ServiceWorkspaceActions,
    device: &DeviceCert,
    name: &str,
) -> CreatedWorkspace {
    let asking = Caller::authenticated(device.user_public_key, device.device_public_key);
    let receipt = settled(host.as_host(device).invoke(&asking, own_request(name)))
        .await
        .expect("the host answers");
    match receipt.status {
        ReceiptStatus::Completed { output } => {
            serde_json::from_value(output.value.expect("the workspace made")).unwrap()
        }
        other => panic!("a host operation completes at once: {other:?}"),
    }
}

/// Put `name` among the people the site the host offers workspaces of their
/// own for let in: the invitation into `home` that names them.
async fn let_in(workspaces: &Workspaces, home: &str, person: [u8; 32]) {
    workspaces
        .issue(
            home,
            workspaces.serving_certificate(),
            IssueInvitationRequest {
                request_id: rand::random(),
                role: Role::Viewer,
                recipient_user_public_key: Some(person),
                invitee_name: None,
                expires_at_ns: tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
}

/// Admit `person`'s device to `workspace` as an Editor, through its owner's
/// pre-admission.
async fn admit(
    workspaces: &Workspaces,
    workspace: &str,
    owner: &DeviceCert,
    person: &DeviceCert,
    seed: u8,
) {
    let token = [seed; 8];
    let issued = workspaces
        .pre_admit(
            owner,
            PreAdmitRequest {
                request_id: [seed; 32],
                grant: signed_pre_admission(workspace, &token),
                recipient_user_public_key: Some(person.user_public_key),
                inviter_name: None,
            },
        )
        .await
        .unwrap();
    let reply = workspaces
        .admit_proven(
            issued.signed_invitation,
            person.clone(),
            rand::random(),
            "Ada",
            Some(token.as_slice()),
        )
        .await;
    assert!(matches!(reply, JoinReply::Admitted { .. }), "{reply:?}");
}

/// Whether the store under `root` keeps `ACCOUNT`.
fn keeps_the_account(root: &std::path::Path) -> bool {
    let email = accounts::AccountTypeId::from(accounts::email::TYPE_ID);
    accounts::AccountStore::load_in(root)
        .list(&email)
        .iter()
        .any(|account| account.id == accounts::AccountId::from(ACCOUNT))
}

/// A person's store kept per person before workspaces kept it, written with
/// the writers that kept it: an account with its sign-in, a provider, and a
/// sealed secret, with the person's own formats and rule beside them.
fn an_old_store(root: &std::path::Path) -> String {
    keep(root, ACCOUNT, PASSWORD);
    let profile = agent_provider_config::ProviderProfile::local_vllm();
    let config = agent_provider_config::ProviderConfig {
        profiles: vec![profile.clone()],
        ..agent_provider_config::ProviderConfig::default()
    };
    save_provider_config(root, &config).unwrap();
    // The settings are written on their own thread; let them reach the disk
    // before the store is copied.
    let settings = settings_store::profile(root);
    settings_store::SettingsSource::flush_pending(&settings);
    let secret = accounts::AccountId::new(SECRET);
    let mut vault = accounts::SecretVault::open_in(root);
    vault.put(&secret, &accounts::Secret::password(SEALED));
    vault.try_save().unwrap();
    std::fs::write(root.join("formats.json"), b"{}").unwrap();
    std::fs::create_dir_all(root.join("telling")).unwrap();
    profile.id
}

#[tokio::test]
async fn a_host_moves_what_it_kept_per_person_into_the_workspaces_they_own() {
    let dir = tempfile::tempdir().unwrap();
    let (state, keys) = (dir.path().join("state"), dir.path().join("keys"));
    // The folder its operator made for the keys: a host makes none.
    std::fs::create_dir(&keys).unwrap();
    let (key, owner) = governor();
    let ada = guest_device().1;
    let bo = guest_device().1;

    // A workspace of Ada's own, and one she is only a member of.
    let (own_workspace, shared) = {
        let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
        let workspaces = server.workspaces().clone();
        workspaces
            .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
            .unwrap();
        let home = workspaces.create("Lounge").await.unwrap().workspace;
        assert!(workspaces.admit_own_through(&home));
        let_in(&workspaces, &home, ada.user_public_key).await;
        let host = ServiceWorkspaceActions::new();
        host.attach(workspaces.clone());
        let own = create_own(&host, &ada, "Ada's workspace").await;
        let shared = workspaces.create("Team").await.unwrap().workspace;
        admit(&workspaces, &shared, &owner, &ada, 3).await;
        assert!(workspaces.owns(&ada.user_public_key, &own.workspace.workspace));
        assert_eq!(
            workspaces.member_role(&shared, ada.user_public_key).await,
            Some(Role::Editor),
            "she is a member of the other, and no owner"
        );
        server.shutdown().await;
        (own.workspace.workspace, shared)
    };

    // The store the host kept for her, per person, as its last version did.
    let people = state.join("workspaces").join("people");
    let hers = people.join(hex::encode(ada.user_public_key));
    let provider_id = an_old_store(&hers);
    assert!(
        hers.join("accounts").join("store.json").exists(),
        "the old store keeps its account"
    );
    // Its key, in the folder the host was told to keep keys apart in.
    let her_key = keys
        .join(hex::encode(ada.user_public_key))
        .join("accounts")
        .join("vault")
        .join("vault.key");
    assert!(
        her_key.exists(),
        "the key the host keeps apart: {her_key:?}"
    );

    // Someone who owns no workspace here kept a store of their own as well.
    let bos_store = people.join(hex::encode(bo.user_public_key));
    keep(&bos_store, "bo@example.test", "bos-password");

    // The host opens again over the same state.
    let server = test_server_keeping_keys_apart(&state, &keys, &key, &owner).await;
    let workspaces = server.workspaces();
    let owned_root = workspaces.workspace_root(&own_workspace);

    // Her account, provider and sealed secret are the workspace's now.
    assert!(
        keeps_the_account(&owned_root),
        "the workspace keeps the account its owner kept"
    );
    let profiles = provider_config(&owned_root).profiles;
    assert_eq!(
        profiles
            .iter()
            .map(|profile| profile.id.as_str())
            .collect::<Vec<_>>(),
        [provider_id.as_str()],
        "the workspace keeps her provider"
    );
    assert_eq!(
        accounts::SecretVault::open_in(&owned_root)
            .get(&accounts::AccountId::new(SECRET))
            .map(|secret| secret.password),
        Some(SEALED.to_owned()),
        "her sealed secret reads back in the workspace's vault"
    );
    let workspace_key = keys
        .join("workspaces")
        .join(hex::encode(own_workspace.as_bytes()))
        .join("accounts")
        .join("vault")
        .join("vault.key");
    assert!(
        workspace_key.exists(),
        "the workspace's own key: {workspace_key:?}"
    );
    assert_ne!(
        std::fs::read(&workspace_key).unwrap(),
        std::fs::read(&her_key).unwrap(),
        "the secret is sealed under the workspace's key, not hers"
    );

    // The workspace she is only a member of got nothing of it.
    let shared_root = workspaces.workspace_root(&shared);
    assert!(
        !shared_root.join("accounts").exists() && !shared_root.join("documents").exists(),
        "a workspace she is only a member of keeps nothing of hers: {shared_root:?}"
    );

    // Her own store left her folder, and only what is hers alone stays.
    let mut left: Vec<String> = std::fs::read_dir(&hers)
        .unwrap()
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(left, ["formats.json", "telling"]);

    // A person who owns no workspace here keeps their store until one is.
    assert!(
        bos_store.join("accounts").join("store.json").exists(),
        "a person with no workspace here keeps their store"
    );

    server.shutdown().await;
}
