use super::tests::{publish_credential, told_by};
use super::*;
use crate::cloud::service::{CloudRequest, InProcessLauncher, ServiceConfig};
use std::path::Path;

fn config(
    root: &Path,
    home: &Path,
    session: &accounts::sign_ins::fixture::Fixture,
) -> ServiceConfig {
    ServiceConfig {
        config_root: Some(root.into()),
        mounts_root: home.join("Remote"),
        state_root: home.join("state"),
        accounts: told_by(session),
        control_socket: None,
        file_socket: None,
    }
}

#[test]
fn pinned_commit_survives_expiry_restart_and_fences_recovery_against_writer() {
    let server = cloud_source_dav::fake::FakeDav::generic();
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut store = accounts::person::AccountList::default();
    let mut account = accounts::Account::new("files-1", "webdav");
    account.config = server.account_config();
    store.keep(&account).unwrap();
    let session = publish_credential(root.path(), "files-1", account);
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        config(root.path(), home.path(), &session),
        Box::new(launcher.clone()),
    );
    service.reconcile();
    let writer = runtime_fs::cloud::import_writer_lock(root.path(), "files-1").unwrap();
    let CloudResponse::ImportPrepared { token } = service.request(
        CloudRequest::PrepareImport {
            account: "files-1".into(),
        },
        tilo_test_env::hang_bound(),
    ) else {
        panic!("preparation failed")
    };
    assert_eq!(
        service.begin_commit_import("files-1", &token),
        CloudResponse::Done
    );
    service.import_holds.get_mut("files-1").unwrap().deadline = Instant::now();
    service.expire_import_holds();
    assert!(service.mounted().is_empty());
    assert!(service.import_holds["files-1"].phase == Phase::Committing);
    service.shutdown();
    drop(service);
    let mut service = CloudService::new(
        config(root.path(), home.path(), &session),
        Box::new(launcher),
    );
    service.reconcile();
    assert!(
        service.mounted().is_empty(),
        "restart released an in-flight metadata writer"
    );
    assert!(matches!(
        service.recover_import("files-1"),
        CloudResponse::Error(_)
    ));
    assert!(matches!(
        service.request(
            CloudRequest::Resume {
                account: "files-1".into()
            },
            tilo_test_env::hang_bound()
        ),
        CloudResponse::Error(_)
    ));
    let mut account = store.account("files-1").unwrap();
    accounts::cloud_types::set_files_mode(
        &mut account.config,
        accounts::cloud_types::FilesMode::Import,
    );
    store.keep(&account).unwrap();
    session.keep_account(account);
    service.reconcile();
    assert!(service.import_holds.contains_key("files-1"));
    assert!(matches!(
        service.complete_import("files-1", "wrong"),
        CloudResponse::Error(_)
    ));
    assert_eq!(
        service.complete_import("files-1", &token),
        CloudResponse::Done
    );
    drop(writer);
    assert!(service.import_holds.is_empty());
    assert_eq!(
        fs::read(home.path().join("state").join(JOURNAL)).unwrap(),
        b"[]"
    );
    service.shutdown();
}
