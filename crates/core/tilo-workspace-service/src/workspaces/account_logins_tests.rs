//! A workspace's mail account signs in with one entry of its vault on every member's device:
//! two members' devices sign in to the same mail server with it, each from its own copy of the
//! vault document; a member whose role leaves Passwords out is refused it in words; one the rules
//! ask first holds no password and reads the account's mail where the vault is; and a file
//! account the cloud service lists signs in by its entry the same way.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::significant_drop_tightening,
    clippy::large_futures,
    clippy::too_many_lines,
    reason = "in a test these are the assertion, and each test keeps both devices to its end"
)]

use std::sync::Arc;

use accounts::person::AccountList;
use ed25519_dalek::SigningKey;
use serde_json::json;
use tilo_account_probe::AccountTester as _;
use tilo_actions::remote::person_accounts;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_actions::remote::{ActionCallError, Caller, InvokeArgument, InvokeRequest};
use tilo_resource::{Id, Replica, Resource as _};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use zeroize::Zeroizing;

use super::account_logins::{
    ASKS_FIRST, AccountLogin, MAY_NOT_USE_PASSWORDS, NOT_NAMED, NOT_ON_A_FOREIGN_DEVICE,
};
use super::fixtures::{TestHost, device_cert, governor, signed_pre_admission, test_host_keyed};

use super::{MemberDevice, PreAdmitRequest, Workspaces};
use crate::own_device::KeyService;

/// The mail account both members read.
const ADDRESS: &str = "team@example.test";
/// Its password: made up, and nobody's.
const PASSWORD: &str = "made-up team password";
/// The app whose engine signs in.
const MAIL: &str = "so.tilo.mail";

/// A stand-in mail server taking the account's one login, and the account's settings that
/// name it.
struct MailServer {
    started: tilo_account_probe::stand_in::Started,
}

impl MailServer {
    fn start() -> Self {
        let started =
            tilo_account_probe::stand_in::started(tilo_account_probe::stand_in::Serving {
                names: vec!["localhost".into()],
                login: ADDRESS.into(),
                password: PASSWORD.into(),
                oauth_token: None,
                imap: "127.0.0.1:0".into(),
                smtp: "127.0.0.1:0".into(),
                imap_wrapped: true,
                smtp_wrapped: false,
                layout: tilo_account_probe::stand_in::Layout::Plain,
                seeded: true,
            })
            .expect("the stand-in mail server listens");
        Self { started }
    }

    fn config(&self) -> accounts::email::AccountConfig {
        accounts::email::AccountConfig {
            display_name: "Team".into(),
            email: ADDRESS.into(),
            imap_host: "localhost".into(),
            imap_port: self.started.imap.port(),
            imap_tls: true,
            smtp_host: "localhost".into(),
            smtp_port: self.started.smtp.port(),
            smtp_tls: true,
            username: ADDRESS.into(),
            ..Default::default()
        }
    }

    fn account(&self) -> accounts::Account {
        let mut account =
            accounts::Account::new(accounts::AccountId::from(ADDRESS), accounts::email::TYPE_ID);
        account.display_name = "Team".into();
        account.config = self.config().to_value().unwrap();
        account
    }

    fn login(&self) -> AccountLogin {
        AccountLogin {
            title: ADDRESS.into(),
            username: ADDRESS.into(),
            places: accounts::email::password_places(&self.config()).unwrap(),
        }
    }

    /// Sign in to both servers with `password`, as Mail's engine does.
    fn signs_in(&self, password: &str) -> Result<String, String> {
        tilo_account_probe::TrustingMailTester {
            reach: tilo_account_probe::imap::Reach::any(),
            extra: vec![self.started.certificate.clone()],
        }
        .test(&self.account(), &accounts::Secret::password(password))
    }
}

/// A workspace served by the owner's device, and a member's device that joined it: two
/// people, each on a device of their own, both holding the workspace's keys as their key
/// services hand them.
struct Members {
    workspace: String,
    owner: Workspaces,
    owner_cert: DeviceCert,
    owner_key: SigningKey,
    member: Workspaces,
    member_cert: DeviceCert,
    member_key: SigningKey,
    _hosts: (TestHost, TestHost),
    _keys: (KeyService, KeyService),
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Members {
    /// `request` as the member's device sends it: signed with its key.
    fn signed(&self, mut request: InvokeRequest) -> InvokeRequest {
        request.signed = Some(Box::new(
            tilo_server_protocol::invocation::sign_invocation_now(
                &self.workspace,
                &request,
                &self.member_cert,
                &self.member_key,
                tilo_clock::unix_now_ns(),
            )
            .expect("the member's device signs its call"),
        ));
        request
    }

    async fn new() -> Self {
        Self::with_member(DeviceCapabilities::member()).await
    }

    /// The owner's workspace, which a device with `capabilities` joined as its member's.
    async fn with_member(capabilities: DeviceCapabilities) -> Self {
        let owner_dir = tempfile::tempdir().unwrap();
        let member_dir = tempfile::tempdir().unwrap();
        let (owner_key, owner_cert) = governor();
        let (serving, owner_keys) =
            test_host_keyed(owner_dir.path(), &owner_key, &owner_cert).await;
        let created = serving.workspaces().create("Team").await.unwrap();
        let workspace = created.workspace;
        let (key, cert) = device_cert(&SigningKey::from_bytes(&[31; 32]), "laptop", capabilities);
        let issued = serving
            .workspaces()
            .pre_admit(
                &owner_cert,
                PreAdmitRequest {
                    request_id: [31; 32],
                    grant: signed_pre_admission(&workspace, &[31; 8]),
                    recipient_user_public_key: Some(cert.user_public_key),
                    inviter_name: None,
                },
            )
            .await
            .unwrap();
        let invitation = tilo_peer_state::join::encode_invitation_text(&issued.signed_invitation);
        let (joined, member_keys) = test_host_keyed(member_dir.path(), &key, &cert).await;
        let member = joined.workspaces().clone();
        member.set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key.clone()),
        }));
        member.join_member(&invitation).await.unwrap();
        Self {
            workspace,
            owner: serving.workspaces().clone(),
            owner_cert,
            owner_key,
            member,
            member_cert: cert,
            member_key: key,
            _hosts: (serving, joined),
            _keys: (owner_keys, member_keys),
            _dirs: (owner_dir, member_dir),
        }
    }

    fn owner_caller(&self) -> Caller {
        Caller::authenticated(
            self.owner_cert.user_public_key,
            self.owner_cert.device_public_key,
        )
        .in_workspace(self.workspace.clone())
    }

    /// Whether the owner's device counts the member's device among the vault's key holders.
    async fn member_holds_the_vaults_keys(&self) -> bool {
        self.owner
            .state(&self.workspace)
            .await
            .unwrap()
            .document_key_holders(
                tilo_app_id::apps::PASSWORDS,
                tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
                tilo_clock::wall(),
            )
            .unwrap()
            .contains(&self.member_cert.device_public_key)
    }

    fn member_caller(&self) -> Caller {
        Caller::authenticated(
            self.member_cert.user_public_key,
            self.member_cert.device_public_key,
        )
        .in_workspace(self.workspace.clone())
    }

    /// The workspace's accounts, made by the owner's device holding `account`: what Settings
    /// writes when the owner adds it.
    async fn keep_account(&self, account: &accounts::Account) {
        let document = collab_types::workspace_document_id(AccountList::KIND);
        let id = collab_types::document::DocumentId::parse(&document).unwrap();
        let mut doc = automerge::AutoCommit::new();
        collaboration::resource::genesis(
            &mut doc,
            &collab_types::document::DocumentCheckout::new(
                id,
                collab_types::document::ResourceType::new(AccountList::KIND),
            ),
            &self.workspace,
            |doc| AccountList::initialize(doc).map_err(|error| error.to_string()),
        )
        .unwrap();
        let saved = {
            let mut replica = Replica::<AccountList>::from_doc(doc)
                .in_app(accounts::person::ACCOUNTS_OWNER, "Settings");
            replica
                .apply_confirmed(AccountList::saving(account))
                .unwrap();
            replica.save()
        };
        let route = tilo_peer_state::document_frame::DocumentRoute {
            app_id: accounts::person::ACCOUNTS_OWNER.into(),
            cluster_id: self.workspace.clone(),
            document: document.clone(),
        };
        let keys = match self.owner.own_route_keys(&route, true).await {
            Some(Ok(keys)) => Some(keys),
            Some(Err(reason)) => panic!("the accounts' keys: {reason}"),
            None => None,
        };
        self.owner
            .own_write(
                &self.workspace,
                (accounts::person::ACCOUNTS_OWNER, &document),
                None,
                &saved,
                true,
                true,
                keys.as_ref(),
            )
            .await
            .unwrap();
    }

    /// The workspace's mail account at `server`, whose password the owner types once on their
    /// device: it goes to the workspace's vault, and the account names the entry.
    async fn owner_keeps_the_password(&self, server: &MailServer) -> ItemRef {
        self.keep_account(&server.account()).await;
        let item = self
            .owner
            .keep_account_password(
                &self.workspace,
                (&self.owner_caller(), None),
                None,
                server.login(),
                Zeroizing::new(PASSWORD.into()),
            )
            .await
            .unwrap();
        self.owner
            .name_account_password(&self.workspace, ADDRESS, &item)
            .await
            .unwrap();
        item
    }

    /// A rule of the workspace that answers the member's every read of Passwords with `answer`:
    /// refused ([`Answer::Deny`], Passwords left out of their role) or asked first.
    async fn rule_passwords_for_the_member(&self, answer: tilo_action_policy::Answer) {
        use tilo_action_policy::{Grant, Grantee, KindSet, TreePath};
        let state = self.owner.workspace(&self.workspace).await.unwrap();
        let mut governed = state.lock().await;
        let refused = Grant::new(
            "no-passwords-for-the-member",
            Grantee::person(hex::encode(self.member_cert.user_public_key)),
            TreePath::workspace(&self.workspace).app(tilo_app_id::apps::PASSWORDS),
            KindSet::ALL,
            answer,
            Grantee::person(hex::encode(self.owner_cert.user_public_key)),
        );
        let key = self.owner_key.clone();
        let now = tilo_clock::unix_now_ns();
        let at = super::system_time(now);
        // As every change of the workspace's rules is made: committed, then settled with its
        // members' devices.
        self.owner
            .administer(
                &mut governed,
                [41; 32],
                [42; 32],
                |peer| {
                    peer.governance_mut()
                        .put_governed_grants(&[refused], &key, at)
                },
                now,
            )
            .await
            .unwrap();
        drop(governed);
        assert!(
            self.owner
                .using_vault(&self.workspace, &self.member_caller(), None, false)
                .await
                .is_err(),
            "the owner's device releases the member no password unasked once the rule is the \
             workspace's"
        );
    }
}

/// The entry the account names in `workspaces`' copy of the accounts, once it names one.
async fn named_entry(workspaces: &Workspaces, workspace: &str) -> ItemRef {
    tilo_test_env::within("the device's copy of the accounts names the entry", async {
        loop {
            if let Ok(held) = workspaces.workspace_accounts(workspace).await
                && let Some(item) = held
                    .accounts
                    .get(&Id::from_raw(ADDRESS))
                    .and_then(|account| accounts::email::password_item(&account.config))
                    .and_then(ItemRef::parse)
            {
                return item;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
}

/// The password `by` is released from `workspaces`' own copy of the vault, once that copy
/// keeps the entry.
async fn released(
    workspaces: &Workspaces,
    workspace: &str,
    by: &Caller,
    app: &str,
    item: &ItemRef,
) -> Result<Zeroizing<String>, String> {
    tilo_test_env::within("the device's copy of the vault keeps the entry", async {
        loop {
            if workspaces.vault_keeps(workspace, item).await == Ok(true) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    workspaces
        .account_secret(workspace, by, app, item)
        .await
        .map(|released| released.secret)
}

#[tokio::test]
async fn two_members_devices_sign_in_to_one_mail_account_with_the_one_vault_entry() {
    let server = Arc::new(
        tokio::task::spawn_blocking(MailServer::start)
            .await
            .unwrap(),
    );
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    let item = members.owner_keeps_the_password(&server).await;

    // Each device reads the account and its entry from its own copy and signs in with it.
    for (device, by) in [
        (&members.owner, members.owner_caller()),
        (&members.member, members.member_caller()),
    ] {
        let named = named_entry(device, &workspace).await;
        assert_eq!(
            named, item,
            "every device reads the one entry the account names"
        );
        let password = released(device, &workspace, &by, MAIL, &named)
            .await
            .expect("a member who may use Passwords is released the account's password");
        let signing = Arc::clone(&server);
        let signed_in = tokio::task::spawn_blocking(move || signing.signs_in(&password))
            .await
            .unwrap();
        assert!(signed_in.is_ok(), "the device signs in: {signed_in:?}");
    }

    // A password typed anew replaces the entry's: the account keeps naming the same entry.
    let again = members
        .member
        .keep_account_password(
            &workspace,
            (&members.member_caller(), None),
            Some(&item),
            server.login(),
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .unwrap();
    assert_eq!(
        again, item,
        "a password typed anew is kept in the entry the account names"
    );

    // Each release is written down beside the entry, saying which app signed in to which
    // account.
    let opened = members.member.open_vault(&workspace).await.unwrap();
    let uses = opened
        .entries()
        .iter()
        .find(|(entry, _)| entry.id.0 == item.id())
        .map(|(_, uses)| uses.clone())
        .unwrap();
    assert!(
        uses.iter()
            .any(|used| used.what == format!("{MAIL} signed in to Team")),
        "the member's release is written down with the app and the account: {uses:?}"
    );

    // An entry no account of the workspace names is released to nobody.
    let unnamed = ItemRef::parse("vault.item:0123456789abcdef0123456789abcdef").unwrap();
    assert_eq!(
        members
            .member
            .account_secret(&workspace, &members.member_caller(), MAIL, &unnamed)
            .await
            .map(|released| released.secret),
        Err(NOT_NAMED.to_owned()),
        "only an account's own entry is released"
    );
}

/// Who made the last change of the document `bytes`, as its stamp names them.
fn last_invoker(bytes: &[u8]) -> tilo_action_policy::Acting {
    let mut doc = automerge::AutoCommit::load(bytes).unwrap();
    let changes = doc.get_changes(&[]);
    let last = changes.last().expect("the document holds changes");
    collab_types::claim::invoker_of(last).expect("every change names who made it")
}

/// What a member asks of the service another member's device runs is written for the member
/// who asked: Bo keeps the account's password and signs it out through Ada's device, and each
/// change that makes names Bo as its invoker, never Ada, whose device wrote it. What Ada's
/// device writes for Ada names Ada.
#[tokio::test]
async fn a_change_a_device_makes_for_another_member_names_the_member_who_asked() {
    let server = Arc::new(
        tokio::task::spawn_blocking(MailServer::start)
            .await
            .unwrap(),
    );
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    let ada = hex::encode(members.owner_cert.user_public_key);
    let bo = hex::encode(members.member_cert.user_public_key);
    members.keep_account(&server.account()).await;
    let vault_invoker = || async {
        let opened = members.owner.open_vault(&workspace).await.unwrap();
        last_invoker(opened.document().expect("the vault is made"))
    };
    let accounts_invoker = || async {
        let bytes = members.owner.accounts_document(&workspace).await.unwrap();
        last_invoker(&bytes.expect("the accounts are made"))
    };

    // Each of Bo's asks reaches Ada's device as the call Bo's device signed.
    let mut keeping = asking(person_accounts::LIST, &serde_json::json!({}));
    keeping.target =
        tilo_actions::remote::person_vault::target(tilo_actions::remote::person_vault::ADD);
    let keeping = members.signed(keeping);
    let item = members
        .owner
        .keep_account_password(
            &workspace,
            (&members.member_caller(), Some(&keeping)),
            None,
            server.login(),
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .unwrap();
    assert_eq!(
        vault_invoker().await.person_id(),
        Some(bo.as_str()),
        "the password Bo typed is kept for Bo"
    );

    members
        .owner
        .name_account_password(&workspace, ADDRESS, &item)
        .await
        .unwrap();
    assert_eq!(
        accounts_invoker().await.person_id(),
        Some(ada.as_str()),
        "what Ada's device writes on its own is Ada's"
    );

    let signing_out = members.signed(asking(
        person_accounts::REMOVE,
        &serde_json::json!({"account": ADDRESS}),
    ));
    assert!(
        members
            .owner
            .forget_workspace_account(
                &workspace,
                (&members.member_caller(), Some(&signing_out)),
                ADDRESS,
            )
            .await
            .unwrap(),
        "the account was there to sign out of"
    );
    assert_eq!(
        accounts_invoker().await.person_id(),
        Some(bo.as_str()),
        "Bo signed the account out"
    );
    assert_eq!(
        vault_invoker().await.person_id(),
        Some(bo.as_str()),
        "Bo's signing out took the password with it"
    );
}

#[tokio::test]
async fn a_member_whose_role_leaves_passwords_out_is_not_released_the_password() {
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    members.owner_keeps_the_password(&server).await;
    let named = named_entry(&members.member, &workspace).await;
    assert!(members.member_holds_the_vaults_keys().await);
    members
        .rule_passwords_for_the_member(tilo_action_policy::Answer::Deny)
        .await;
    assert!(
        !members.member_holds_the_vaults_keys().await,
        "the vault's next keys leave the member's device out"
    );

    // Once the member's device reads the rule, it is refused, in the words its person sees.
    let member = members.member.clone();
    let by = members.member_caller();
    let refused = tilo_test_env::within("the member's device reads the rule", async {
        loop {
            match member.account_secret(&workspace, &by, MAIL, &named).await {
                Err(error) if error == MAY_NOT_USE_PASSWORDS => return error,
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    assert!(!refused.contains(PASSWORD));
    assert!(
        members
            .member
            .keep_account_password(
                &workspace,
                (&by, None),
                Some(&named),
                server.login(),
                Zeroizing::new("another made-up password".into()),
            )
            .await
            .is_err_and(|error| error == MAY_NOT_USE_PASSWORDS),
        "nor may they change it"
    );
    // The owner still signs in.
    assert_eq!(
        members
            .owner
            .account_secret(&workspace, &members.owner_caller(), MAIL, &named)
            .await
            .unwrap()
            .secret
            .as_str(),
        PASSWORD
    );
}

/// What Mail on a member's device asks of the account's mail where the vault is: `action` of
/// the workspace's accounts, with `argument`.
fn asking(action: &str, argument: &serde_json::Value) -> InvokeRequest {
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: person_accounts::target(action),
        argument: InvokeArgument::Json(argument.as_object().unwrap().clone()),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// A device of somebody else's, which may read the workspace's content, is handed no account's
/// password: the decision that releases one says so in its person's words.
#[tokio::test]
async fn a_device_not_its_persons_own_is_not_released_the_password() {
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let mut capabilities = DeviceCapabilities::member();
    capabilities.foreign = true;
    let members = Members::with_member(capabilities).await;
    let workspace = members.workspace.clone();
    members.owner_keeps_the_password(&server).await;
    let named = named_entry(&members.owner, &workspace).await;
    assert_eq!(
        members
            .owner
            .account_secret(&workspace, &members.member_caller(), MAIL, &named)
            .await
            .map(|released| released.secret),
        Err(NOT_ON_A_FOREIGN_DEVICE.to_owned())
    );
}

#[tokio::test]
async fn a_file_accounts_password_signs_in_by_its_entry_on_a_members_device() {
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    let mut account = accounts::Account::new("files-1", accounts::cloud_types::WEBDAV);
    account.display_name = "Team files".into();
    account.config = serde_json::json!({
        "server_url": "https://dav.example.test/remote.php/dav/files/ada/",
        "username": "ada",
    });
    members.keep_account(&account).await;

    // The owner types the password once, on their device: the workspace's vault keeps it, and
    // the account names the entry.
    let item = members
        .owner
        .keep_account_password(
            &workspace,
            (&members.owner_caller(), None),
            None,
            AccountLogin {
                title: "Team files".into(),
                username: "ada".into(),
                places: super::person_settings::kept_sign_ins::places_of(&account).unwrap(),
            },
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .expect("the owner keeps the password in the workspace's vault");
    members
        .owner
        .name_account_password(&workspace, "files-1", &item)
        .await
        .expect("the account names its entry");

    // The member's device lists the account with the entry it names, as the cloud service reads
    // it, and signs in with that entry.
    let is_files =
        |kept: &accounts::Account| kept.type_id.as_str() == accounts::cloud_types::WEBDAV;
    let listed = tilo_test_env::within("the member's copy names the file account's entry", async {
        loop {
            if let Some(item) = members
                .member
                .held_accounts(is_files)
                .await
                .into_iter()
                .find(|(held, kept, _)| *held == workspace && kept.id.as_str() == "files-1")
                .and_then(|(_, kept, _)| {
                    accounts::email::password_item(&kept.config).and_then(ItemRef::parse)
                })
            {
                return item;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert_eq!(
        listed, item,
        "the member lists the account with the entry the owner kept"
    );
    let signed_in =
        tilo_test_env::within("the member's copy of the vault keeps the entry", async {
            loop {
                match members
                    .member
                    .account_secret(
                        &workspace,
                        &members.member_caller(),
                        tilo_app_id::apps::FILE_EXPLORER,
                        &item,
                    )
                    .await
                {
                    Ok(released) => return released,
                    Err(_) => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
                }
            }
        })
        .await;
    assert_eq!(signed_in.secret.as_str(), PASSWORD);
    let opened = members.member.open_vault(&workspace).await.unwrap();
    let uses = opened
        .entries()
        .iter()
        .find(|(entry, _)| entry.id.0 == item.id())
        .map(|(_, uses)| uses.clone())
        .unwrap();
    let files = tilo_app_id::apps::FILE_EXPLORER;
    assert!(
        uses.iter()
            .any(|used| used.what == format!("{files} signed in to Team files")),
        "the release is written down with what signed in: {uses:?}"
    );
}

#[tokio::test]
async fn an_account_a_providers_page_signed_in_is_released_its_access_token_alone() {
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    let mut account = server.account();
    let mut config = server.config();
    config.auth = accounts::email::AuthMethod::OAuth;
    account.config = config.to_value().unwrap();
    account.config["provider"] = "google".into();
    members.keep_account(&account).await;
    // The grant a sign-in on Google's own page left, kept in the account's entry: its access
    // token good for the next hour, and the refresh token that renews it.
    let tokens = tilo_oauth::TokenSet {
        access_token: "made-up access token".into(),
        refresh_token: "made-up refresh token".into(),
        expires_in: 3600,
        ..Default::default()
    }
    .stamp_now();
    let grant = account_credentials::oauth_secret(&tokens).token.unwrap();
    let item = members
        .owner
        .keep_account_password(
            &workspace,
            (&members.owner_caller(), None),
            None,
            server.login(),
            Zeroizing::new(grant),
        )
        .await
        .unwrap();
    members
        .owner
        .name_account_password(&workspace, ADDRESS, &item)
        .await
        .unwrap();

    let named = named_entry(&members.member, &workspace).await;
    let token = released(
        &members.member,
        &workspace,
        &members.member_caller(),
        MAIL,
        &named,
    )
    .await
    .expect("a member who may use Passwords signs in to the account");
    assert_eq!(
        token.as_str(),
        "made-up access token",
        "the device signs in with the access token, and never holds the grant"
    );
    assert!(
        !token.contains("made-up refresh token") && !token.contains("refresh_token"),
        "nothing the device is released carries the refresh token: {}",
        token.as_str()
    );
}

/// The vault's route, as each device seals and opens it.
fn vault_route(workspace: &str) -> tilo_peer_state::document_frame::DocumentRoute {
    tilo_peer_state::document_frame::DocumentRoute {
        app_id: tilo_app_id::apps::PASSWORDS.into(),
        cluster_id: workspace.to_owned(),
        document: tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT.into(),
    }
}

/// The vault's keys as `device` holds them once they are no longer `held`; the group and
/// generation it held last when they never move on.
async fn vault_keys_after(
    device: &Workspaces,
    workspace: &str,
    held: &key_group::ContentKeys,
) -> key_group::ContentKeys {
    let mut last = None;
    let waited = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let answer = device.own_route_keys(&vault_route(workspace), false).await;
            if let Some(Ok(keys)) = &answer
                && !keys.same_write_key(held)
            {
                return keys.clone();
            }
            last = Some(answer.map(|keys| keys.map(|keys| (keys.workspace(), keys.epoch()))));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    waited.unwrap_or_else(|_| {
        panic!(
            "the vault stayed sealed with keys the member's device held ({:?} at {}): the \
             device's vault keys stayed at {last:?}",
            held.workspace(),
            held.epoch()
        )
    })
}

/// A member whose rules come to refuse Passwords opens nothing the vault keeps after that: the
/// vault's key group leaves their device out and moves to a new generation, which their device is
/// never handed. Real key services on both devices, so what each holds is what a device holds.
#[tokio::test]
async fn a_member_refused_passwords_opens_nothing_the_vault_keeps_after() {
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    members.owner_keeps_the_password(&server).await;
    // Both devices settle on the keys the vault is sealed with.
    let mut last = None;
    let held = tokio::time::timeout(std::time::Duration::from_secs(60), async {
        loop {
            let route = vault_route(&workspace);
            let owner = members.owner.own_route_keys(&route, false).await;
            let member = members.member.own_route_keys(&route, false).await;
            if let (Some(Ok(owner)), Some(Ok(member))) = (&owner, &member)
                && owner.same_write_key(member)
            {
                return owner.clone();
            }
            let at = |answer: Option<Result<key_group::ContentKeys, String>>| {
                answer.map(|held| held.map(|keys| (keys.workspace(), keys.epoch())))
            };
            last = Some((at(owner), at(member)));
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| {
        panic!("the two devices never held the vault's keys alike: owner and member at {last:?}")
    });

    members
        .rule_passwords_for_the_member(tilo_action_policy::Answer::Deny)
        .await;
    // The owner keeps another password: a write of the vault after the member lost it.
    tokio::time::timeout(
        std::time::Duration::from_secs(60),
        members.owner.keep_account_password(
            &workspace,
            (&members.owner_caller(), None),
            None,
            server.login(),
            Zeroizing::new("a password kept after".into()),
        ),
    )
    .await
    .expect("the owner's device keeps a password after the member lost Passwords")
    .unwrap();
    let next = vault_keys_after(&members.owner, &workspace, &held).await;
    let artifact = collab_types::document_artifact_id(
        tilo_app_id::apps::PASSWORDS,
        &workspace,
        tilo_action_policy::roles::WORKSPACE_VAULT_DOCUMENT,
    );
    let (metadata, sealed) = next
        .seal_content(
            artifact,
            collab_types::ContentRetention::Delta,
            b"a password kept after",
        )
        .unwrap();
    assert!(
        held.open_content(artifact, &metadata, &sealed).is_err(),
        "the keys the member's device held open nothing sealed after"
    );
    // The groups settle in milliseconds; keys handed to the member's device would be there long
    // before this.
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    if let Some(Ok(keys)) = members
        .member
        .own_route_keys(&vault_route(&workspace), false)
        .await
    {
        assert!(
            keys.open_content(artifact, &metadata, &sealed).is_err(),
            "the member's device holds no key of what is sealed after"
        );
    }
}

/// A branch of a workspace starts with everything its source holds, the passwords its vault keeps
/// included: its own device opens them there with the branch's own keys. A
/// workspace is branched where agents run.
#[cfg(feature = "agents")]
#[tokio::test(flavor = "multi_thread")]
async fn a_branch_opens_the_passwords_its_source_kept() {
    const BRANCH: &str = "6f2b8a51-6d0f-4d4e-9d6c-5d3b3f1f2a11";
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = governor();
    let (host, _keys) = test_host_keyed(dir.path(), &key, &cert).await;
    let workspaces = host.workspaces();
    workspaces
        .offer_own_workspaces(super::OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let source = workspaces.create("Template").await.unwrap().workspace;
    let by = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(source.clone());
    let item = workspaces
        .keep_account_password(
            &source,
            (&by, None),
            None,
            server.login(),
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .unwrap();

    workspaces
        .branch_own(&source, BRANCH, "Task", [5; 32])
        .await
        .map_err(|not| not.refusal())
        .unwrap();
    let opened = workspaces
        .open_vault(BRANCH)
        .await
        .expect("the branch's vault opens on its device");
    assert!(
        opened.entry(item.id()).is_some(),
        "the branch keeps the password its source kept: {opened:?}"
    );
}

/// The bridge, installed on the member's device with what it needs, its process never run.
struct HeldBridge;

impl crate::work::HostConnections for HeldBridge {
    fn connections(&self) -> Vec<(String, Vec<String>)> {
        vec![(BRIDGE.to_owned(), Vec::new())]
    }
    fn installed(&self, app: &str) -> bool {
        app == BRIDGE
    }
    fn gives(&self, _: &str, _: &str, _: &str) -> bool {
        true
    }
    fn on_power(&self) -> bool {
        true
    }
    fn start(&self, _: &str, _: &str) -> Result<(), String> {
        Ok(())
    }
    fn runs(&self, _: &str, _: &str) -> bool {
        false
    }
    fn stop(&self, _: &str, _: &str) {}
}

/// The app whose held connection asks for a vault item.
const BRIDGE: &str = "so.tilo.bridge";

/// The member's device holds the bridge's connection, but the workspace's rules leave Passwords
/// out of its person's role: it releases the bridge no vault item, and says why in the vault's
/// words.
#[tokio::test]
async fn a_device_that_may_not_read_the_vault_releases_its_connection_nothing() {
    use super::vault_release::{AppProcess, device_may_not_read};
    use crate::work::{Order, Turn, takes_turn_in};

    let members = Members::new().await;
    let workspace = members.workspace.clone();
    members
        .rule_passwords_for_the_member(tilo_action_policy::Answer::Deny)
        .await;
    members.member.run_connections(Arc::new(HeldBridge));
    // The member's is the only device here that runs the bridge: it waits for no other.
    let at_once = Order {
        server_grace: std::time::Duration::ZERO,
        other_grace: std::time::Duration::ZERO,
    };
    takes_turn_in(
        &members.member,
        &workspace,
        &Turn::connection(BRIDGE, Vec::new()),
        at_once,
    )
    .await
    .expect("the member's device takes the bridge's connection");
    let item = ItemRef::parse("vault.item:0123456789abcdef0123456789abcdef").unwrap();
    let process = AppProcess::connection(BRIDGE);
    let call = InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: tilo_actions::remote::person_vault::release_target(&item),
        argument: InvokeArgument::None,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    };
    let member = members.member.clone();
    let refused = tilo_test_env::within("the member's device reads the rule", async {
        loop {
            match member
                .release_to((BRIDGE, &workspace), &process, &call)
                .await
            {
                Err(ActionCallError::Refused { reason })
                    if reason == device_may_not_read(BRIDGE, MAY_NOT_USE_PASSWORDS) =>
                {
                    return reason;
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(20)).await,
            }
        }
    })
    .await;
    assert!(refused.contains(MAY_NOT_USE_PASSWORDS));
}

/// Every step the account actions declare they take in the accounts record is
/// one of that record's own actions, in Settings' record of that kind.
#[test]
fn the_account_actions_declare_steps_the_accounts_record_takes() {
    use tilo_actions::remote::person_accounts;
    use tilo_resource::Resource as _;
    let verbs: Vec<_> = AccountList::declarations()
        .into_iter()
        .map(|declared| declared.id)
        .collect();
    let mut declared = 0;
    for (resource, action) in [
        (person_accounts::RESOURCE, person_accounts::ADD_MAIL),
        (
            person_accounts::RESOURCE,
            person_accounts::ADD_MAIL_WITH_ITEM,
        ),
        (person_accounts::RESOURCE, person_accounts::BEGIN_SIGN_IN),
        (person_accounts::RESOURCE, person_accounts::RECONNECT),
        (person_accounts::RESOURCE, person_accounts::REMOVE),
        (person_accounts::RESOURCE, person_accounts::SET_SERVICES),
    ] {
        for effect in person_accounts::effects(resource, action)
            .into_iter()
            .filter(|effect| effect.kind == AccountList::KIND)
        {
            assert_eq!(effect.app, accounts::person::ACCOUNTS_OWNER, "{action}");
            assert!(verbs.contains(&effect.verb.as_str()), "{action}: {effect}");
            declared += 1;
        }
    }
    assert_eq!(
        declared, 7,
        "every account action that writes the record declares its step"
    );
    assert!(
        person_accounts::effects(person_accounts::RESOURCE, person_accounts::LIST).is_empty(),
        "a read writes nothing"
    );
}
