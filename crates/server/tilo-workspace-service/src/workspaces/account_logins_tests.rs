//! A workspace's mail account signs in with one entry of its vault on every member's device:
//! two members' devices sign in to the same mail server with it, each from its own copy of the
//! vault document; a member whose role leaves Passwords out is refused it in words; and a file
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

use accounts::person::PersonAccounts;
use ed25519_dalek::SigningKey;
use tilo_account_probe::AccountTester as _;
use tilo_actions::remote::Caller;
use tilo_actions::remote::person_vault::ItemRef;
use tilo_resource::{Id, Replica, Resource as _};
use tilo_user_identity::{DeviceCapabilities, DeviceCert};
use zeroize::Zeroizing;

use super::account_logins::{AccountLogin, MAY_NOT_USE_PASSWORDS, NOT_NAMED};
use super::fixtures::{
    TestGroupKeys, TestHost, device_cert, governor, signed_pre_admission, test_host_holding,
};
use super::{MemberDevice, PreAdmitRequest, Workspaces};

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
    _hosts: (TestHost, TestHost),
    _dirs: (tempfile::TempDir, tempfile::TempDir),
}

impl Members {
    async fn new() -> Self {
        let keys = Arc::new(TestGroupKeys::default());
        let owner_dir = tempfile::tempdir().unwrap();
        let member_dir = tempfile::tempdir().unwrap();
        let (owner_key, owner_cert) = governor();
        let serving =
            test_host_holding(owner_dir.path(), &owner_key, &owner_cert, Arc::clone(&keys)).await;
        let created = serving.workspaces().create("Team").await.unwrap();
        let workspace = created.workspace;
        let (key, cert) = device_cert(
            &SigningKey::from_bytes(&[31; 32]),
            "laptop",
            DeviceCapabilities::member(),
        );
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
        let joined = test_host_holding(member_dir.path(), &key, &cert, keys).await;
        let member = joined.workspaces().clone();
        member.set_member_device(Arc::new(MemberDevice {
            cert: cert.clone(),
            signer: Arc::new(key),
        }));
        member.join_member(&invitation).await.unwrap();
        Self {
            workspace,
            owner: serving.workspaces().clone(),
            owner_cert,
            owner_key,
            member,
            member_cert: cert,
            _hosts: (serving, joined),
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
        let document = collab_types::workspace_document_id(PersonAccounts::KIND);
        let id = collab_types::document::DocumentId::parse(&document).unwrap();
        let mut doc = automerge::AutoCommit::new();
        collaboration::resource::genesis(
            &mut doc,
            &collab_types::document::DocumentCheckout::new(
                id,
                collab_types::document::ResourceType::new(PersonAccounts::KIND),
            ),
            &self.workspace,
            |doc| PersonAccounts::initialize(doc).map_err(|error| error.to_string()),
        )
        .unwrap();
        let saved = {
            let mut replica = Replica::<PersonAccounts>::from_doc(doc)
                .in_app(accounts::person::ACCOUNTS_OWNER, "Settings");
            replica
                .apply_confirmed(PersonAccounts::saving(account))
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

    /// Leave Passwords out of the member's role: a rule of the workspace that refuses them
    /// every read of it.
    async fn refuse_passwords_to_the_member(&self) {
        use tilo_action_policy::{Answer, Grant, Grantee, KindSet, TreePath};
        let state = self.owner.workspace(&self.workspace).await.unwrap();
        let mut governed = state.lock().await;
        let refused = Grant::new(
            "no-passwords-for-the-member",
            Grantee::person(hex::encode(self.member_cert.user_public_key)),
            TreePath::workspace(&self.workspace).app(tilo_app_id::apps::PASSWORDS),
            KindSet::ALL,
            Answer::Deny,
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
            !self
                .owner
                .may_use_vault(&self.workspace, &self.member_caller())
                .await,
            "the owner's device refuses the member Passwords once the rule is the workspace's"
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
        .account_secret(workspace, by, MAIL, item)
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
    members.keep_account(&server.account()).await;

    // The owner types the password once, on their device: it goes to the workspace's vault,
    // and the account names the entry.
    let item = members
        .owner
        .keep_account_password(
            &workspace,
            &members.owner_caller(),
            None,
            server.login(),
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .expect("the owner keeps the password in the workspace's vault");
    members
        .owner
        .name_account_password(&workspace, ADDRESS, &item)
        .await
        .expect("the account names its entry");

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
        let password = released(device, &workspace, &by, &named)
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
            &members.member_caller(),
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

#[tokio::test]
async fn a_member_whose_role_leaves_passwords_out_is_not_released_the_password() {
    let server = tokio::task::spawn_blocking(MailServer::start)
        .await
        .unwrap();
    let members = Members::new().await;
    let workspace = members.workspace.clone();
    members.keep_account(&server.account()).await;
    let item = members
        .owner
        .keep_account_password(
            &workspace,
            &members.owner_caller(),
            None,
            server.login(),
            Zeroizing::new(PASSWORD.into()),
        )
        .await
        .unwrap();
    members
        .owner
        .name_account_password(&workspace, ADDRESS, &item)
        .await
        .unwrap();
    let named = named_entry(&members.member, &workspace).await;
    members.refuse_passwords_to_the_member().await;

    // Once the member's device reads the rule, it is refused, in the words its person sees,
    // though the device holds the vault's keys.
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
                &by,
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
            &members.owner_caller(),
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
                .find(|(held, kept)| *held == workspace && kept.id.as_str() == "files-1")
                .and_then(|(_, kept)| {
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
