use super::*;
use crate::setup::{portable_transfer::ExportReview, prepared_identity::PreparedIdentity};
use std::{fs, os::unix::fs::PermissionsExt};
use tilo_identity_host::provisioning::IdentityStorage;
const PASSWORD: &[u8] = b"several unrelated installation words";
fn private(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn package(root: &Path, staging: &Path, name: &str) -> PathBuf {
    let path = root.join(name);
    ExportReview::review(staging, &path, Purpose::PreparedSetup, LIMITS)
        .unwrap()
        .export(Zeroizing::new(PASSWORD.to_vec()), &AtomicBool::new(false))
        .unwrap();
    path
}
#[test]
fn real_prepared_setup_authenticates_exact_identity_accounts_then_imports_fresh_custody() {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let staging = root.join("staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let source = root.join("source-accounts");
    let connector = Connector::new(source.clone());
    let selected = connector
        .connect_webdav(
            "Files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("selected-private-password".into()),
        )
        .unwrap();
    let other = connector
        .connect_webdav(
            "Other",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("other-private-password".into()),
        )
        .unwrap();
    connector
        .export_selected_accounts(
            std::slice::from_ref(&selected),
            &staging.join(ACCOUNTS_FILE),
            PASSWORD,
            b"operation-1",
        )
        .unwrap();
    Manifest::new(
        "operation-1".into(),
        identity.public_key(),
        vec![selected.as_str().into()],
    )
    .unwrap()
    .write(&staging)
    .unwrap();
    let account_before = fs::read(accounts::store_path_in(&source)).unwrap();
    let vault_before = fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap();
    let exported = package(&root, &staging, "prepared.sealed");
    let reviewed = PreparedSetupReview::open(
        &exported,
        PASSWORD,
        &root.join("quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    // New target is a sibling of quarantine under the same private parent.
    let target = root.join("target");
    let identity_path = reviewed.import_identity(&target, PASSWORD).unwrap();
    let target_before = fs::read(&identity_path).unwrap();
    assert_eq!(
        reviewed.import_identity(&target, PASSWORD).unwrap(),
        identity_path
    );
    assert!(
        reviewed
            .import_identity(&target, b"wrong password")
            .is_err()
    );
    assert_eq!(fs::read(&identity_path).unwrap(), target_before);
    assert_eq!(
        runtime_identity::load_from_path(identity_path)
            .unwrap()
            .group_public_key(),
        identity.public_key()
    );

    let identity_path = reviewed
        .import_identity(&target.join("identities"), PASSWORD)
        .unwrap();
    assert_eq!(
        runtime_identity::load_from_path(identity_path)
            .unwrap()
            .group_public_key(),
        identity.public_key()
    );
    let config = target.join("tilo");
    let unauthenticated = target.join("unauthenticated-accounts");
    assert!(
        reviewed
            .import_accounts(&unauthenticated, b"wrong password")
            .is_err()
    );
    assert!(!unauthenticated.exists());
    assert_eq!(
        reviewed.import_accounts(&config, PASSWORD).unwrap(),
        vec![selected.clone()]
    );
    assert_eq!(
        fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        reviewed.import_accounts(&config, PASSWORD).unwrap(),
        vec![selected.clone()]
    );
    let store = accounts::AccountStore::load_in(&config);
    assert!(store.get(&selected).is_some());
    assert!(store.get(&other).is_none());
    assert_eq!(
        fs::read(accounts::store_path_in(&source)).unwrap(),
        account_before
    );
    assert_eq!(
        fs::read(accounts::vault_dir_in(&source).join("vault.enc")).unwrap(),
        vault_before
    );
    // A changed accepted tree is refused before a second target is created.
    fs::write(reviewed.root.join(MANIFEST_FILE), b"{}").unwrap();
    assert!(
        reviewed
            .import_identity(&target.join("changed"), PASSWORD)
            .is_err()
    );
    assert!(!target.join("changed").exists());
}
#[test]
fn malformed_manifest_extra_member_wrong_identity_and_selection_refuse_before_adoption() {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let staging = root.join("staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    Manifest::new("operation-1".into(), identity.public_key(), vec![])
        .unwrap()
        .write(&staging)
        .unwrap();
    let good = package(&root, &staging, "good.sealed");
    assert!(
        PreparedSetupReview::open(
            &good,
            b"wrong",
            &root.join("wrong-password"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    fs::write(staging.join("extra"), b"must not be adopted").unwrap();
    let extra = package(&root, &staging, "extra.sealed");
    assert!(
        PreparedSetupReview::open(
            &extra,
            PASSWORD,
            &root.join("extra-quarantine"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    fs::remove_file(staging.join("extra")).unwrap();
    fs::remove_file(staging.join(MANIFEST_FILE)).unwrap();
    Manifest::new("operation-1".into(), [0; 32], vec![])
        .unwrap()
        .write(&staging)
        .unwrap();
    let wrong = package(&root, &staging, "wrong-identity.sealed");
    assert!(
        PreparedSetupReview::open(
            &wrong,
            PASSWORD,
            &root.join("wrong-identity"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    let connector = Connector::new(root.join("accounts-source"));
    let actual = connector
        .connect_webdav(
            "Files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("private-password".into()),
        )
        .unwrap();
    connector
        .export_selected_accounts(
            &[actual],
            &staging.join(ACCOUNTS_FILE),
            PASSWORD,
            b"operation-1",
        )
        .unwrap();
    fs::remove_file(staging.join(MANIFEST_FILE)).unwrap();
    Manifest::new(
        "operation-1".into(),
        identity.public_key(),
        vec!["different-selected-account".into()],
    )
    .unwrap()
    .write(&staging)
    .unwrap();
    let mismatch = package(&root, &staging, "selection.sealed");
    assert!(
        PreparedSetupReview::open(
            &mismatch,
            PASSWORD,
            &root.join("selection"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!root.join("target").exists());
    assert!(Manifest::new("\n".into(), identity.public_key(), vec![]).is_err());
    assert!(
        Manifest::new(
            "operation".into(),
            identity.public_key(),
            vec!["same".into(), "same".into()]
        )
        .is_err()
    );
    assert!(serde_json::from_str::<Manifest>(r#"{"schema":1,"setup_binding":"op","identity_public_key":[0],"account_ids":[],"extra":true}"#).is_err());
}

#[test]
fn authenticated_applications_only_plan_never_authorizes_original_documents() {
    struct NoInstalledRefs;
    impl crate::setup::flatpak_transfer::Runner for NoInstalledRefs {
        fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
            if matches!(args.first().map(String::as_str), Some("list" | "remotes")) {
                Ok(String::new())
            } else {
                Err("No installed references in this isolated source".into())
            }
        }
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let _facts = crate::setup::discovery_fixture::source_fixture(&root);
    let home = root.join("home");
    fs::create_dir_all(home.join(".var/app/example.app")).unwrap();
    fs::write(
        home.join(".var/app/example.app/state"),
        b"original app state",
    )
    .unwrap();
    fs::write(home.join("document"), b"not selected").unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let mut plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: false,
            flatpak: true,
            accounts: false,
        },
    )
    .unwrap();
    let inventory = crate::setup::flatpak_transfer::HandoffInventory::capture(
        &NoInstalledRefs,
        &home,
        vec![],
        |_| Err("No repository sources in this fixture".into()),
    )
    .unwrap();
    plan.flatpak_inventory = Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap());
    let mut manifest = Manifest::new("apps-only".into(), [0; 32], vec![]).unwrap();
    manifest.retained_home = Some(RetainedHomePlan::new(plan.clone()).unwrap());
    let staging = root.join("staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    manifest.identity_public_key = identity.public_key();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    manifest.write(&staging).unwrap();
    let encrypted = package(&root, &staging, "apps-only.sealed");
    let review = PreparedSetupReview::open(
        &encrypted,
        PASSWORD,
        &root.join("review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(
        review.manifest().retained_home.as_ref().unwrap().as_plan(),
        &plan
    );
    assert!(
        !review
            .manifest()
            .retained_home
            .as_ref()
            .unwrap()
            .as_plan()
            .categories
            .home
    );
    assert_eq!(
        plan.home_policy,
        crate::setup::migration_handoff::HomePolicy::Copy
    );
    assert!(review.manifest().portable_home.is_none());
    assert_eq!(fs::read(home.join("document")).unwrap(), b"not selected");
    assert_eq!(
        fs::read(home.join(".var/app/example.app/state")).unwrap(),
        b"original app state"
    );
}

struct LargeInventoryCli {
    root: PathBuf,
}
impl crate::setup::flatpak_transfer::Runner for LargeInventoryCli {
    fn run(&self, _home: &Path, args: &[String]) -> Result<String, String> {
        match args.first().map(String::as_str) {
            Some("list") if args.get(2).map(String::as_str) == Some("--app") => {
                let mut list = String::new();
                for index in 0..50 {
                    list.push_str(&format!(
                        "org.example.Prepared{index:03}/x86_64/stable\tflathub\twork\t{} {index}\n",
                        "Public application name ".repeat(6)
                    ));
                }
                Ok(list)
            }
            Some("list") if args.get(1).map(String::as_str) == Some("--user") => Ok(String::new()),
            Some("list") => {
                Ok("org.example.Platform/x86_64/stable\tflathub\twork\tPlatform\n".into())
            }
            Some("remotes") if args.get(1).map(String::as_str) == Some("--user") => {
                Ok(String::new())
            }
            Some("remotes") => Ok("flathub\thttps://example.org/repo/\t\n".into()),
            Some("info") if args.contains(&"--show-runtime".into()) => {
                Ok("org.example.Platform/x86_64/stable".into())
            }
            Some("info") if args.contains(&"--show-extensions".into()) => {
                Ok(args.last().unwrap().clone())
            }
            Some("info") => {
                let reference = args.last().ok_or("Missing fixture reference")?;
                Ok(self
                    .root
                    .join(reference)
                    .join("a".repeat(64))
                    .to_string_lossy()
                    .into_owned())
            }
            _ => Err("Unexpected source inventory query".into()),
        }
    }
}
fn large_flatpak_inventory(
    home: &Path,
    operation: &str,
) -> crate::setup::flatpak_transfer::HandoffInventory {
    use crate::setup::{flatpak_transfer, migration_handoff};
    let root = home.parent().unwrap().join("system-flatpak");
    fs::create_dir_all(root.join("repo")).unwrap();
    fs::write(root.join("repo/config"), b"[core]\nmode=bare-user-only\n[remote \"flathub\"]\nurl=https://example.org/repo/\ngpg-verify=true\n").unwrap();
    fs::write(
        root.join("repo/flathub.trustedkeys.gpg"),
        b"original public repository trust",
    )
    .unwrap();
    for reference in ["runtime/org.example.Platform/x86_64/stable".to_owned()]
        .into_iter()
        .chain((0..50).map(|index| format!("app/org.example.Prepared{index:03}/x86_64/stable")))
    {
        let parent = root.join(reference);
        let deployment = parent.join("a".repeat(64));
        fs::create_dir_all(&deployment).unwrap();
        fs::write(deployment.join("deploy"), b"original deployment metadata").unwrap();
        std::os::unix::fs::symlink("a".repeat(64), parent.join("active")).unwrap();
    }
    let runner = LargeInventoryCli { root };
    let installed = flatpak_transfer::discover(&runner, home).unwrap();
    assert_eq!(installed.refs.len(), 51);
    flatpak_transfer::HandoffInventory::capture(&runner, home, installed.refs, |root| {
        migration_handoff::capture(
            root,
            operation,
            migration_handoff::Categories {
                home: false,
                flatpak: true,
                accounts: false,
            },
        )
        .and_then(|plan| plan.bytes())
        .map_err(|error| error.to_string())
    })
    .unwrap()
}
#[test]
fn authenticated_large_manifest_preserves_actual_retained_home_metadata() {
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let _facts = crate::setup::discovery_fixture::source_fixture(&root);
    let home = root.join("home");
    fs::create_dir(&home).unwrap();
    let operation = uuid::Uuid::new_v4().to_string();
    let inventory = large_flatpak_inventory(&home, &operation);
    let mut plan = crate::setup::migration_handoff::capture(
        &home,
        &operation,
        crate::setup::migration_handoff::Categories {
            home: true,
            flatpak: true,
            accounts: false,
        },
    )
    .unwrap();
    plan.home_policy = crate::setup::migration_handoff::HomePolicy::AccessReadWrite;
    plan.flatpak_inventory = Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap());
    plan.home_exclusions = vec![crate::setup::migration_store::NativePath::encode(
        std::path::Path::new("Documents/Private"),
    )];
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    let mut manifest =
        Manifest::new("large-retained-plan".into(), identity.public_key(), vec![]).unwrap();
    manifest.retained_home = Some(RetainedHomePlan::new(plan).unwrap());
    let staging = root.join("staging");
    private(&staging);
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let compact = serde_json::to_vec(&manifest).unwrap();
    assert!(
        compact.len() > 32 * 1024,
        "actual Flatpak metadata must cross the former limit"
    );
    assert!(compact.len() < MANIFEST_LIMIT as usize);
    manifest.write(&staging).unwrap();
    assert_eq!(fs::read(staging.join(MANIFEST_FILE)).unwrap(), compact);
    let encrypted = package(&root, &staging, "large-manifest.sealed");
    let review = PreparedSetupReview::open(
        &encrypted,
        PASSWORD,
        &root.join("review"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(review.manifest(), &manifest);
    assert_eq!(fs::read(review.root.join(MANIFEST_FILE)).unwrap(), compact);
    assert_eq!(
        review
            .manifest()
            .retained_home
            .as_ref()
            .unwrap()
            .as_plan()
            .flatpak_inventory,
        Some(serde_json::from_slice(&inventory.bytes().unwrap()).unwrap())
    );
    assert_eq!(
        review
            .manifest()
            .retained_home
            .as_ref()
            .unwrap()
            .as_plan()
            .home_exclusion_paths()
            .unwrap(),
        vec![std::path::PathBuf::from("Documents/Private")],
        "the sealed manifest keeps the reviewed folder exclusion"
    );
}

#[test]
fn protected_accounts_require_authenticated_manifest_protocol_before_adoption() {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let staging = root.join("staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let original = root.join("original");
    let source = Connector::new(original.clone());
    let id = source
        .connect_webdav(
            "Files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("private-source-password".into()),
        )
        .unwrap();
    let bytes = fs::read(accounts::vault_dir_in(&original).join("vault.enc")).unwrap();
    let copied = root.join("copied");
    let connector = Connector::new(copied);
    connector.import_tilo(&original, &id).unwrap();
    connector
        .export_selected_accounts(
            std::slice::from_ref(&id),
            &staging.join(ACCOUNTS_FILE),
            PASSWORD,
            b"protected-operation",
        )
        .unwrap();
    let mut manifest = Manifest::new(
        "protected-operation".into(),
        identity.public_key(),
        vec![id.as_str().into()],
    )
    .unwrap();
    manifest.write(&staging).unwrap();
    let missing = package(&root, &staging, "missing-requirement.sealed");
    assert!(
        PreparedSetupReview::open(
            &missing,
            PASSWORD,
            &root.join("missing-quarantine"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert!(!root.join("target-identity").exists());
    assert!(!root.join("target-accounts").exists());
    fs::remove_file(staging.join(MANIFEST_FILE)).unwrap();
    manifest.account_custody_protocol = Some(1);
    manifest.write(&staging).unwrap();
    let valid = package(&root, &staging, "protected.sealed");
    let review = PreparedSetupReview::open(
        &valid,
        PASSWORD,
        &root.join("valid-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(review.manifest().account_custody_protocol, Some(1));
    // The current canonical custodian genuinely consumes the resealed grant;
    // image/first-login admission separately verifies its installed counterpart.
    let target = root.join("target-accounts");
    assert_eq!(
        review.import_accounts(&target, PASSWORD).unwrap(),
        vec![id.clone()]
    );
    account_custody::Custodian::for_root(Some(target.clone()))
        .publish(&id, false)
        .unwrap();
    let Some(accounts::HandoffCredential::Basic { username, password }) =
        accounts::HandoffStore::open_in(&target).read(&id).unwrap()
    else {
        panic!("missing canonical credential")
    };
    assert_eq!(username, "ada");
    assert_eq!(password.as_str(), "private-source-password");
    assert_eq!(
        fs::read(accounts::vault_dir_in(&original).join("vault.enc")).unwrap(),
        bytes
    );
}

#[test]
fn prepared_setup_authenticates_separate_selected_session_before_any_adoption() {
    use crate::setup::service_sessions::{PreparedSessions, source::SourceGrant};
    use browser_continuity::{Provider, sessions::Service};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let staging = root.path().join("staging");
    private(&staging);
    let source = root.path().join("browser-source");
    private(&source);
    let db = rusqlite::Connection::open(source.join("cookies.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=17; CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER); INSERT INTO moz_cookies VALUES(1,'.slack.com','d','bound-session-secret','/',2000000123000,1,1,256,'',0,0);").unwrap();
    drop(db);
    let before = fs::read(source.join("cookies.sqlite")).unwrap();
    let identity = PreparedIdentity::create(
        &root.path().join("identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let sessions = PreparedSessions::capture(
        identity.public_key(),
        "selected-operation".into(),
        vec![(
            SourceGrant::new(fs::File::open(&source).unwrap(), Provider::Firefox).unwrap(),
            Service::Slack,
        )],
    )
    .unwrap();
    let (artifact, declaration) = sessions.seal(PASSWORD).unwrap();
    fs::write(staging.join(SERVICE_SESSIONS_FILE), artifact.as_bytes()).unwrap();
    fs::set_permissions(
        staging.join(SERVICE_SESSIONS_FILE),
        fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut manifest =
        Manifest::new("selected-operation".into(), identity.public_key(), vec![]).unwrap();
    manifest.service_sessions = Some(declaration.clone());
    manifest.write(&staging).unwrap();
    let sealed_package = package(root.path(), &staging, "selected-setup.sealed");
    let review = PreparedSetupReview::open(
        &sealed_package,
        PASSWORD,
        &root.path().join("quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let target = root.path().join("target-config");
    assert!(
        review
            .import_service_sessions(&target, b"wrong password")
            .is_err()
    );
    assert!(!target.exists());
    review.import_service_sessions(&target, PASSWORD).unwrap();
    let selected = account_custody::Custodian::for_root(Some(target))
        .service_session(
            &AccountId::from(declaration.sessions[0].id.clone()),
            identity.public_key(),
            "selected-operation",
        )
        .unwrap();
    assert_eq!(selected.cookies[0].value, "bound-session-secret");
    assert_eq!(fs::read(source.join("cookies.sqlite")).unwrap(), before);
    let malformed = root.path().join("malformed");
    private(&malformed);
    fs::copy(staging.join(IDENTITY_FILE), malformed.join(IDENTITY_FILE)).unwrap();
    fs::copy(
        staging.join(SERVICE_SESSIONS_FILE),
        malformed.join(SERVICE_SESSIONS_FILE),
    )
    .unwrap();
    manifest.service_sessions.as_mut().unwrap().sessions[0].service = Service::Discord;
    manifest.write(&malformed).unwrap();
    let sealed_package = package(root.path(), &malformed, "wrong-service.sealed");
    assert!(
        PreparedSetupReview::open(
            &sealed_package,
            PASSWORD,
            &root.path().join("refused-quarantine"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(fs::read(source.join("cookies.sqlite")).unwrap(), before);
}

fn offline_home_fixture(root: &Path) -> (Manifest, PathBuf, PathBuf, AccountId) {
    let dav_server = crate::setup::account_transfer::dav_readiness::test_server();
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.join("home");
    private(&home);
    fs::write(home.join("document"), b"original documents stay here").unwrap();
    let caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home);
    let operation = uuid::Uuid::new_v4().to_string();
    // A deterministic user-authenticated request fixture; kernel source
    // authority is qualified only by the opt-in GPT-backed native test.
    let source: crate::setup::migration_handoff::Plan = serde_json::from_value(serde_json::json!({
        "schema": 1, "operation_id": operation,
        "categories": {"home": true, "flatpak": false, "accounts": true},
        "home": {"volume": {
            "disk_guid": "11111111-1111-4111-8111-111111111111",
            "disk_bytes": 64u64 * 1024 * 1024, "sector_bytes": 512,
            "partition_guid": "22222222-2222-4222-8222-222222222222",
            "first_lba": 2048, "last_lba": 65535,
            "filesystem_uuid": "33333333-3333-4333-8333-333333333333",
            "filesystem": "ext4"
        }, "relative": crate::setup::migration_store::NativePath::encode(Path::new("home/person")),
        "inode": 1234, "original_uid": caller.uid(), "subvolume": null}
    }))
    .unwrap();
    let identity = PreparedIdentity::create(
        &root.join("identity"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    let connector = Connector::new(root.join("accounts"));
    let account = connector
        .connect_webdav(
            "Work files",
            &format!("{}/dav", dav_server.url()),
            "ada",
            Zeroizing::new("original-account-secret".into()),
        )
        .unwrap();
    let mut manifest = Manifest::new(
        operation.clone(),
        identity.public_key(),
        vec![account.as_str().into()],
    )
    .unwrap();
    manifest.portable_home = Some(PortableHomePlan {
        schema: 1,
        selection: PortableHomeSelection::FullHome,
        applications: false,
    });
    let (_media_directory, _media_path, raw_media) = crate::setup::test_support::media::disk();
    let media = crate::media_layout::provision(
        &raw_media,
        4 * 1024 * 1024,
        64 * 1024 * 1024,
        "reviewed-fixture-medium",
        &|| false,
    )
    .unwrap();
    assert_eq!(
        crate::media_layout::binding(&raw_media, 64 * 1024 * 1024, "reviewed-fixture-medium")
            .unwrap(),
        media
    );
    assert!(
        crate::media_layout::binding(
            &raw_media,
            64 * 1024 * 1024 - 512,
            "reviewed-fixture-medium"
        )
        .is_err()
    );
    let declaration = OfflineHomePlan::new(
        source,
        operation.clone(),
        identity.public_key(),
        media,
        caller,
        1024 * 1024,
    )
    .unwrap();
    manifest.offline_home = Some(declaration);
    let staging = root.join("staging");
    private(&staging);
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    connector
        .export_selected_accounts(
            std::slice::from_ref(&account),
            &staging.join(ACCOUNTS_FILE),
            PASSWORD,
            operation.as_bytes(),
        )
        .unwrap();
    manifest.write(&staging).unwrap();
    let ciphertext = package(root, &staging, "pending.sealed");
    drop(identity);
    drop(connector);
    (manifest, ciphertext, staging, account)
}
fn assert_pending_native_consumers_refuse(root: &Path, ciphertext: &Path, original: &[u8]) {
    let refused_identity = root.join("refused-identity");
    let refused_accounts = root.join("refused-accounts");
    let public_receipt = root.join("setup-receipt.json");
    fs::write(
        &public_receipt,
        serde_json::to_vec(&serde_json::json!({
            "schema":1, "sealed_bytes":original.len(),
            "ciphertext_sha256":hex::encode(Sha256::digest(original)),
        }))
        .unwrap(),
    )
    .unwrap();
    fs::set_permissions(&public_receipt, fs::Permissions::from_mode(0o600)).unwrap();
    let mut native_output = Vec::new();
    let native_arguments = [
        std::ffi::OsString::from("--package"),
        ciphertext.to_path_buf().into_os_string(),
        std::ffi::OsString::from("--receipt"),
        public_receipt.into_os_string(),
        std::ffi::OsString::from("--quarantine"),
        root.join("native-refused").into_os_string(),
    ];
    assert!(
        crate::setup::prepared_setup_helper::authenticate::run(
            native_arguments,
            &mut std::io::Cursor::new(PASSWORD),
            &mut native_output,
        )
        .is_err()
    );
    assert!(
        native_output.is_empty(),
        "pending export must not produce install authentication"
    );
    let mut first_use_output = Vec::new();
    let first_use_arguments = [
        std::ffi::OsString::from("--package"),
        ciphertext.to_path_buf().into_os_string(),
        std::ffi::OsString::from("--quarantine"),
        root.join("first-use-refused").into_os_string(),
        std::ffi::OsString::from("--identity-dir"),
        refused_identity.clone().into_os_string(),
        std::ffi::OsString::from("--config-root"),
        refused_accounts.clone().into_os_string(),
    ];
    assert!(
        crate::setup::prepared_setup_helper::run(
            first_use_arguments,
            &mut std::io::Cursor::new(PASSWORD),
            &mut first_use_output,
        )
        .is_err()
    );
    assert!(first_use_output.is_empty());
    assert!(!refused_identity.exists());
    assert!(!refused_accounts.exists());
}
fn assert_offline_declaration_cannot_split(manifest: &Manifest, staging: &Path) {
    let mut missing_caller = serde_json::to_value(manifest).unwrap();
    missing_caller["offline_home"]
        .as_object_mut()
        .unwrap()
        .remove("caller");
    assert!(serde_json::from_value::<Manifest>(missing_caller).is_err());
    let mut wrong_caller = serde_json::to_value(manifest).unwrap();
    wrong_caller["offline_home"]["caller"]["uid"] = serde_json::json!(u32::MAX);
    assert!(
        serde_json::from_value::<Manifest>(wrong_caller)
            .unwrap()
            .validate()
            .is_err()
    );
    let mut split = manifest.clone();
    split.portable_home = None;
    assert!(split.write(staging).is_err());
    let mut split = manifest.clone();
    split.offline_home.as_mut().unwrap().identity_public_key = [9; 32];
    assert!(split.write(staging).is_err());
    let mut split = manifest.clone();
    split.offline_home.as_mut().unwrap().setup_binding = "another-setup".into();
    assert!(split.write(staging).is_err());
    let mut split = manifest.clone();
    split.portable_home.as_mut().unwrap().applications = true;
    assert!(split.write(staging).is_err());
    let mut split = manifest.clone();
    split.offline_home.as_mut().unwrap().source.operation_id = uuid::Uuid::new_v4().to_string();
    assert!(split.write(staging).is_err());
}
#[test]
fn offline_home_crypto_reopen_preserves_exact_selection_and_refuses_split_declaration() {
    if rustix::process::geteuid().is_root() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::chown_to(1),
            "as root the fixture gives the original Home to a non-root owner"
        );
    }
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let _facts = crate::setup::discovery_fixture::source_fixture(&root);
    let (manifest, ciphertext, staging, account) = offline_home_fixture(&root);
    let declaration = manifest.offline_home.as_ref().unwrap().clone();
    let home = root.join("home");
    let original = fs::read(&ciphertext).unwrap();
    let review = PreparedSetupReview::open(
        &ciphertext,
        PASSWORD,
        &root.join("reopened"),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(review.manifest(), &manifest);
    assert_eq!(review.manifest().offline_home.as_ref(), Some(&declaration));
    assert_eq!(
        review.pending_offline_declaration().unwrap_err().kind(),
        io::ErrorKind::Unsupported,
        "authenticated shape-only caller metadata has no protected source witness"
    );
    assert_eq!(
        review.require_completed_export().unwrap_err().kind(),
        io::ErrorKind::Unsupported
    );
    let refused_identity = root.join("refused-identity");
    let refused_accounts = root.join("refused-accounts");
    assert!(
        review
            .validate_destinations(&refused_identity, &refused_accounts)
            .is_err()
    );
    assert!(review.import_identity(&refused_identity, PASSWORD).is_err());
    assert!(review.import_accounts(&refused_accounts, PASSWORD).is_err());
    assert!(!refused_identity.exists());
    assert!(!refused_accounts.exists());
    assert_pending_native_consumers_refuse(&root, &ciphertext, &original);
    assert_eq!(review.manifest().account_ids, vec![account.as_str()]);
    assert_eq!(
        fs::read(home.join("document")).unwrap(),
        b"original documents stay here"
    );
    assert_eq!(fs::read(&ciphertext).unwrap(), original);
    assert!(
        !fs::read(staging.join(MANIFEST_FILE))
            .unwrap()
            .windows(PASSWORD.len())
            .any(|bytes| bytes == PASSWORD)
    );
    drop(review);
    assert_offline_declaration_cannot_split(&manifest, &staging);
    let mut changed = original;
    let last = changed.last_mut().unwrap();
    *last ^= 1;
    fs::write(&ciphertext, &changed).unwrap();
    assert!(
        PreparedSetupReview::open(
            &ciphertext,
            PASSWORD,
            &root.join("changed"),
            &AtomicBool::new(false)
        )
        .is_err()
    );
    assert_eq!(fs::read(&ciphertext).unwrap(), changed);
    let mut legacy = manifest;
    legacy.offline_home = None;
    let value = serde_json::to_value(&legacy).unwrap();
    assert!(value.get("offline_home").is_none());
    assert!(
        serde_json::from_value::<Manifest>(value)
            .unwrap()
            .validate()
            .is_ok()
    );
}

#[test]
fn pending_home_without_account_consent_refuses_selected_service_sessions() {
    if rustix::process::geteuid().is_root() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::chown_to(1),
            "as root the fixture gives the original Home to a non-root owner"
        );
    }
    use crate::setup::service_sessions::{PreparedSessions, source::SourceGrant};
    use browser_continuity::{Provider, sessions::Service};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::tempdir().unwrap();
    let (mut manifest, _, _, _) = offline_home_fixture(root.path());
    manifest.account_ids.clear();
    manifest.account_custody_protocol = None;
    let source = root.path().join("selected-service-source");
    private(&source);
    let db = rusqlite::Connection::open(source.join("cookies.sqlite")).unwrap();
    db.execute_batch("PRAGMA user_version=17; CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY,host TEXT,name TEXT,value TEXT,path TEXT,expiry INTEGER,isSecure INTEGER,isHttpOnly INTEGER,sameSite INTEGER,originAttributes TEXT,isPartitionedAttributeSet INTEGER,inBrowserElement INTEGER); INSERT INTO moz_cookies VALUES(1,'.slack.com','d','synthetic-selected-session','/',2000000123000,1,1,256,'',0,0);").unwrap();
    drop(db);
    let selected = PreparedSessions::capture(
        manifest.identity_public_key,
        manifest.setup_binding.clone(),
        vec![(
            SourceGrant::new(fs::File::open(&source).unwrap(), Provider::Firefox).unwrap(),
            Service::Slack,
        )],
    )
    .unwrap();
    let (_, declaration) = selected.seal(PASSWORD).unwrap();
    manifest.service_sessions = Some(declaration);
    manifest.validate().unwrap();
    manifest
        .offline_home
        .as_mut()
        .unwrap()
        .source
        .categories
        .accounts = false;
    assert!(
        manifest.validate().is_err(),
        "pending Home cannot silently retain accounts outside consent"
    );
}

#[test]
#[cfg(target_os = "linux")]
fn authenticated_geometry_keeps_home_only_shape_and_refuses_unadmitted_inventory_marker() {
    if rustix::process::geteuid().is_root() {
        tilo_test_env::require_capability!(
            Chown,
            tilo_test_env::chown_to(1),
            "as root the fixture gives the original Home to a non-root owner"
        );
    }
    use crate::setup::migration_handoff::{self, Categories};
    let temp = tempfile::Builder::new()
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()
        .unwrap();
    let home = temp.path().join("home");
    private(&home);
    let caller = crate::setup::home_capture::original_caller::fixture_source_owner(&home);
    let operation = uuid::Uuid::new_v4().to_string();
    let source = migration_handoff::capture_fixture(
        &home,
        &operation,
        Categories {
            home: true,
            ..Default::default()
        },
    )
    .unwrap();
    let media = crate::media_layout::MediaPartitionBinding {
        schema: 1,
        target_stable_id: "geometry-fixture".into(),
        disk_guid: uuid::Uuid::new_v4().to_string(),
        partition_guid: uuid::Uuid::new_v4().to_string(),
        number: 3,
        first_lba: 8192,
        last_lba: 16383,
        sector_bytes: 512,
        profile_offset: 4096,
    };
    let declaration = OfflineHomePlan::new(
        source.clone(),
        "geometry-fixture".into(),
        [9; 32],
        media.clone(),
        caller.clone(),
        1024 * 1024,
    )
    .unwrap();
    assert_eq!(
        declaration.authenticated_source_plans().unwrap(),
        crate::setup::source_geometry::original_source_plans(&source).unwrap()
    );
    // A syntactically valid public marker is not the protected original inventory.
    let mut marked = source;
    marked.categories.flatpak = true;
    marked.flatpak_inventory = Some(
        serde_json::json!({"kind":"protected-original-flatpak","schema":1,"operation":operation,"len":128,"sha256":vec![1u8;32]}),
    );
    let declaration = OfflineHomePlan::new(
        marked,
        "geometry-fixture".into(),
        [9; 32],
        media,
        caller,
        1024 * 1024,
    )
    .unwrap();
    let error = declaration.authenticated_source_plans().unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    assert!(!temp.path().join("tilo-imager").exists());
}

#[cfg(target_os = "linux")]
#[test]
fn selected_mail_only_reaches_the_target_mail_store_through_the_authenticated_package() {
    use crate::account_sources::SourceRoots;
    use crate::account_transfer::{PasswordSealedThunderbird, ThunderbirdSelection};
    let mut env = tilo_test_env::ScopedEnv::new();
    env.set(runtime_identity::FILE_SECRETS_ENV, "1");
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let root = root.path();
    let home = root.join("mail-source");
    private(&home);
    let profile = home.join(".thunderbird/abc.default");
    std::fs::create_dir_all(profile.join("ImapMail/imap.example.test")).unwrap();
    std::fs::write(
        home.join(".thunderbird/profiles.ini"),
        "[Profile0]\nName=Personal\nIsRelative=1\nPath=abc.default\n",
    )
    .unwrap();
    std::fs::write(
        profile.join("prefs.js"),
        r#"user_pref("mail.accountmanager.accounts", "account1");
user_pref("mail.account.account1.server", "server1");
user_pref("mail.account.account1.identities", "id1");
user_pref("mail.server.server1.type", "imap");
user_pref("mail.server.server1.hostname", "imap.example.test");
user_pref("mail.server.server1.directory-rel", "[ProfD]ImapMail/imap.example.test");
user_pref("mail.identity.id1.fullName", "Personal");
user_pref("ldap_2.servers.history.filename", "abook.sqlite");
user_pref("ldap_2.servers.history.description", "Personal addresses");
user_pref("calendar.registry.personal.name", "Personal calendar");
user_pref("calendar.registry.personal.filename", "calendar-data.local.sqlite");
"#,
    )
    .unwrap();
    let message = |id: &str, subject: &str| {
        format!(
            "From - Mon Jan 01 00:00:00 2024\r\nFrom: Ada <ada@example.test>\r\nSubject: {subject}\r\nDate: Mon, 01 Jan 2024 00:00:00 +0000\r\nMessage-ID: <{id}@example.test>\r\n\r\nBody.\r\n"
        )
    };
    std::fs::write(
        profile.join("ImapMail/imap.example.test/Inbox"),
        message("kept", "Kept mail"),
    )
    .unwrap();
    std::fs::write(
        profile.join("ImapMail/imap.example.test/Drafts"),
        message("excluded", "Excluded draft"),
    )
    .unwrap();
    std::fs::write(profile.join("abook.sqlite"), b"selected address book").unwrap();
    std::fs::write(
        profile.join("calendar-data.local.sqlite"),
        b"selected calendar",
    )
    .unwrap();
    let inventory = crate::account_sources::discover(&SourceRoots {
        home: home.clone(),
        config: home.join(".config"),
    });
    let reviewed = inventory.thunderbird_reviewed[0].clone();
    let drafts = reviewed.accounts[0]
        .folders
        .iter()
        .find(|folder| folder.inventory.hierarchy.last().map(String::as_str) == Some("Drafts"))
        .unwrap()
        .inventory
        .source_id
        .clone();
    let mut selection = ThunderbirdSelection::all(&reviewed);
    selection.accounts[0]
        .folders
        .retain(|folder| folder != &drafts);
    selection.validate(&reviewed).unwrap();
    let artifact =
        PasswordSealedThunderbird::seal(&reviewed, &selection, PASSWORD, b"mail-operation")
            .unwrap();
    assert!(
        !String::from_utf8_lossy(artifact.as_bytes()).contains("Kept mail"),
        "the carrier is ciphertext"
    );
    assert_eq!(
        PasswordSealedThunderbird::from_bytes(artifact.as_bytes())
            .unwrap()
            .open(b"a completely different password", b"mail-operation")
            .unwrap_err(),
        crate::account_transfer::ConnectionError::CredentialUnavailable
    );
    let staging = root.join("mail-staging");
    private(&staging);
    let identity = PreparedIdentity::create(
        &root.join("mail-identity-source"),
        "Ada",
        [1, 2, 3],
        IdentityStorage::Files,
    )
    .unwrap();
    identity
        .export(
            &staging.join(IDENTITY_FILE),
            Zeroizing::new(String::from_utf8(PASSWORD.to_vec()).unwrap()),
        )
        .unwrap();
    let filename = "mail-selected.sealed";
    std::fs::write(staging.join(filename), artifact.as_bytes()).unwrap();
    std::fs::set_permissions(
        staging.join(filename),
        std::fs::Permissions::from_mode(0o600),
    )
    .unwrap();
    let mut manifest =
        Manifest::new("mail-operation".into(), identity.public_key(), vec![]).unwrap();
    manifest.mail.push(MailHandoff {
        filename: filename.into(),
        selection,
        sealed_bytes: artifact.as_bytes().len() as u64,
        ciphertext_sha256: Sha256::digest(artifact.as_bytes()).into(),
    });
    manifest.write(&staging).unwrap();
    let sealed_package = package(root, &staging, "selected-mail.sealed");
    let review = PreparedSetupReview::open(
        &sealed_package,
        PASSWORD,
        &root.join("mail-quarantine"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let target = root.join("mail-target");
    private(&target);
    assert!(
        review
            .import_mail(&target, b"a completely different password")
            .is_err(),
        "the target mail store is untouched without the preparation password"
    );
    assert!(!target.join(".config/tilo").exists());
    let receipts = review.import_mail(&target, PASSWORD).unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(receipts[0].address_books, 1);
    assert_eq!(receipts[0].calendars, 1);
    let account = &receipts[0].accounts[0];
    assert_eq!(
        account.folders.len(),
        1,
        "the excluded folder is not imported"
    );
    let cache = target.join(".cache/so.tilo.mail/mail");
    let mut found = String::new();
    fn collect(path: &Path, found: &mut String) {
        for entry in std::fs::read_dir(path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                collect(&entry.path(), found);
            } else if let Ok(text) = std::fs::read_to_string(entry.path()) {
                found.push_str(&text);
            }
        }
    }
    if cache.exists() {
        collect(&cache, &mut found);
    }
    assert!(found.contains("Kept mail"));
    assert!(
        !found.contains("Excluded draft"),
        "an excluded folder must never reach the target Mail store"
    );
    let retained = target.join(".local/share/tilo/mail-import/thunderbird");
    let mut retained_text = String::new();
    if retained.exists() {
        collect(&retained, &mut retained_text);
    }
    assert!(!retained_text.contains("Excluded draft"));
    assert_eq!(
        std::fs::read(profile.join("ImapMail/imap.example.test/Drafts")).unwrap(),
        message("excluded", "Excluded draft").into_bytes(),
        "the excluded original stays untouched"
    );
    let again = review.import_mail(&target, PASSWORD).unwrap();
    assert!(again[0].accounts.iter().all(|account| !account.created));
}
