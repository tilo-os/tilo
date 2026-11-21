use super::*;
use crate::cloud::service::{Arc, CloudRequest, InProcessLauncher, ServiceConfig, mpsc};
use accounts::sign_ins::fixture::Fixture;
use cloud_source_dav::fake::{FakeDav, PASSWORD};
use std::path::Path;

/// A stand-in session telling the accounts `root` keeps, which keeps the fake server's password
/// as the sign-in of account `files-1`.
pub(super) fn publish_credential(root: &Path) -> Fixture {
    let session = Fixture::serve(root, root);
    session.keep("files-1", PASSWORD);
    session
}

/// The accounts `session` tells, as the cloud service reads them.
pub(super) fn told_by(session: &Fixture) -> Arc<dyn crate::cloud::AccountSource> {
    struct NoRefresh;
    impl accounts::CredentialRefresher for NoRefresh {
        fn refresh(&self, _: &AccountId, _: bool) -> Result<(), accounts::RefreshError> {
            Err(accounts::RefreshError::NeedsAuthentication)
        }
    }
    Arc::new(crate::cloud::SessionAccounts::new(
        session.socket(),
        None,
        Arc::new(NoRefresh),
    ))
}

#[test]
fn expired_preparation_remounts_without_changing_pause_or_queued_data() {
    let server = FakeDav::generic();
    server.put("a.txt", b"original");
    let root = tempfile::tempdir().unwrap();
    let home = tempfile::tempdir().unwrap();
    let mut store = AccountStore::load_in(root.path());
    let mut account = accounts::Account::new("files-1", "webdav");
    account.display_name = "Files".into();
    account.config = server.account_config();
    store.upsert(account);
    store.save_in(root.path()).unwrap();
    let session = publish_credential(root.path());
    let launcher = InProcessLauncher::default();
    let mut service = CloudService::new(
        ServiceConfig {
            config_root: Some(root.path().into()),
            mounts_root: home.path().join("Remote"),
            state_root: home.path().join("state"),
            accounts: told_by(&session),
            control_socket: None,
            file_socket: None,
        },
        Box::new(launcher.clone()),
    );
    service.reconcile();
    assert_eq!(
        service.request(
            CloudRequest::Pause {
                account: "files-1".into()
            },
            tilo_test_env::hang_bound()
        ),
        CloudResponse::Done
    );
    let CloudResponse::ImportPrepared { token } = service.request(
        CloudRequest::PrepareImport {
            account: "files-1".into(),
        },
        tilo_test_env::hang_bound(),
    ) else {
        panic!("preparation did not finish")
    };
    assert!(launcher.volume("files-1").is_none());
    service.import_holds.get_mut("files-1").unwrap().deadline = Instant::now();
    service.expire_import_holds();
    let volume = launcher.volume("files-1").unwrap();
    assert!(volume.is_paused());
    volume.sync_once().unwrap();
    let ino = volume.resolve("a.txt").unwrap();
    let writer = volume.open_file(ino, true, false, false).unwrap();
    volume.write(writer, 0, b"retained").unwrap();
    volume.release(writer).unwrap();
    assert_eq!(volume.pending(), 1);
    assert!(matches!(
        service.request(
            CloudRequest::AbortImport {
                account: "files-1".into(),
                token: token.clone(),
            },
            tilo_test_env::hang_bound()
        ),
        CloudResponse::Error(_)
    ));
    assert_eq!(volume.pending(), 1);
    assert!(matches!(
        service.request(
            CloudRequest::BeginCommitImport {
                account: "files-1".into(),
                token
            },
            tilo_test_env::hang_bound(),
        ),
        CloudResponse::Error(_)
    ));
    assert_eq!(volume.pending(), 1);
    volume.upload_pending().unwrap();
    assert_eq!(server.get("a.txt").unwrap(), b"retained");

    expire_before_freeze_reply(&mut service);
    assert_eq!(service.mounted().len(), 1);
    assert!(launcher.volume("files-1").unwrap().is_paused());
    service.shutdown();
}

fn expire_before_freeze_reply(service: &mut CloudService) {
    let (answer, answered) = mpsc::channel();
    service.control(
        CloudRequest::PrepareImport {
            account: "files-1".into(),
        },
        answer,
    );
    service.import_holds.get_mut("files-1").unwrap().deadline = Instant::now();
    service.expire_import_holds();
    loop {
        match answered.try_recv() {
            Ok(response) => {
                assert!(matches!(response, CloudResponse::Error(_)));
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {
                let signal = service
                    .inbox
                    .recv_timeout(tilo_test_env::hang_bound())
                    .unwrap();
                service.handle(signal);
            }
            Err(error) => panic!("preparation reply was lost: {error}"),
        }
    }
}
