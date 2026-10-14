use super::*;

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
        crate::cloud_jobs::run(&op, None).reason(),
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
