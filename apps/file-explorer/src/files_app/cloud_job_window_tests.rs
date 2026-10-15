use super::*;

#[test]
fn unhosted_cloud_import_without_issuer_settles_without_contacting_source() {
    let mut env = tilo_test_env::ScopedEnv::new();
    let root = tempfile::tempdir().unwrap();
    env.set("XDG_RUNTIME_DIR", root.path());
    let socket = tilo_cloud_files::cloud::files_socket_in(root.path());
    std::fs::create_dir_all(socket.parent().unwrap()).unwrap();
    let listener = std::os::unix::net::UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    // Portal services alone are not a checked import issuer.
    let (services, _) = ui::host::HostServices::recording();
    let _host = ui::host::enter(&services);
    let mut files = FileExplorerWidget::with_persistence(None, Some(root.path().to_owned()));
    assert!(!files.cloud_fixture_consent);
    let folder = root.path().display().to_string();
    let entry = ImportEntry {
        id: "native".into(),
        name: "Copy.docx".into(),
        kind: ImportKind::NativeExport,
        revision: None,
        size: None,
        content_writable: false,
    };
    let op = crate::file_ops::FileOp::CloudImport {
        job_id: uuid::Uuid::new_v4().to_string(),
        account: "account".into(),
        destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty()).unwrap(),
        entries: vec![entry],
        folder,
    };
    files.queue(
        op,
        OpFollowUp::Paste {
            count: 1,
            cut: false,
        },
    );
    let deadline = Instant::now() + std::time::Duration::from_secs(5);
    while files.cloud.transfer.is_some() {
        assert!(Instant::now() < deadline);
        files.poll_cloud();
        std::thread::yield_now();
    }
    assert_eq!(files.local_outcomes.len(), 1);
    let outcome = files.local_outcomes.values().next().unwrap();
    assert_eq!(
        outcome.reason(),
        Some("only the host person can approve a personal cloud import")
    );
    assert!(outcome.result().is_empty());
    assert_eq!(files.cloud.error.as_deref(), outcome.reason());
    assert!(!root.path().join("Copy.docx").exists());
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
fn two_windows_cancel_only_their_queued_attempt_and_keep_its_status() {
    let root = tempfile::tempdir().unwrap();
    let mut first = FileExplorerWidget::with_persistence(None, Some(root.path().to_owned()));
    let mut second = FileExplorerWidget::with_persistence(None, Some(root.path().to_owned()));
    let first_id = uuid::Uuid::new_v4().to_string();
    let second_id = uuid::Uuid::new_v4().to_string();
    first.cloud.job_id = Some(first_id.clone());
    second.cloud.job_id = Some(second_id.clone());
    crate::cloud_jobs::register(&first_id);
    crate::cloud_jobs::register(&second_id);
    first.run_effect(FilesEffect::CancelCloud).unwrap();
    let folder = root.path().display().to_string();
    let op = crate::file_ops::FileOp::CloudImport {
        job_id: first_id,
        account: "account".into(),
        entries: vec![],
        destination: crate::cloud_transfer::pin_destination(&folder, std::iter::empty()).unwrap(),
        folder,
    };
    // Execution after queued cancellation cannot clear the request or contact a provider.
    assert_eq!(
        crate::cloud_jobs::run(&op, crate::cloud_jobs::fixture_consent(&op)).reason(),
        Some("cloud import cancelled")
    );
    first.poll_cloud();
    second.poll_cloud();
    assert!(
        matches!(first.run_effect(FilesEffect::CloudProgress).unwrap(),
        Ran::Done(Some(value)) if value["phase"] == "cancelled")
    );
    assert!(
        matches!(second.run_effect(FilesEffect::CloudProgress).unwrap(),
        Ran::Done(Some(value)) if value["phase"] == "")
    );
    second.run_effect(FilesEffect::CancelCloud).unwrap();
    assert_eq!(first.cloud.progress.phase, "cancelled");
    assert!(crate::cloud_jobs::snapshot(&second_id).phase.is_empty());
}
