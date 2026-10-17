use super::*;

#[test]
fn progress_fraction_handles_full_width_counters_and_empty_totals() {
    for (bytes, total, expected) in [
        (0, 0, 0.0),
        (1, 0, 0.0),
        (0, u64::MAX, 0.0),
        (1, 2, 0.5),
        (u64::MAX / 2, u64::MAX, 0.5),
        (u64::MAX, u64::MAX, 1.0),
        (u64::MAX, 1, 1.0),
    ] {
        let fraction = progress_fraction(bytes, total);
        assert!((0.0..=1.0).contains(&fraction));
        assert!((fraction - expected).abs() < 1e-9);
    }
}

#[test]
fn cancellation_delivery_retries_without_mutating_request() {
    let work = tempfile::tempdir().unwrap();
    let job = work.path().join("job");
    let cancel = job.join("cancel");
    let mut sent = false;
    assert!(relay_cancellation(&|| true, &cancel, &mut sent).is_err());
    assert!(!sent);
    std::fs::create_dir(&job).unwrap();
    let request = job.join("request.json");
    std::fs::write(&request, b"digest-bound request").unwrap();
    relay_cancellation(&|| true, &cancel, &mut sent).unwrap();
    assert!(sent);
    assert!(cancel.is_file());
    assert_eq!(std::fs::read(request).unwrap(), b"digest-bound request");
}

#[cfg(unix)]
#[test]
fn relay_reaps_child_before_returning_progress_read_error() {
    let work = tempfile::tempdir().unwrap();
    let mut child = Command::new("sh")
        .args(["-c", "sleep 0.05"])
        .spawn()
        .unwrap();
    let result = relay_helper_progress(
        &mut child,
        work.path(),
        &work.path().join("cancel"),
        &|| false,
        &mut |_| {},
    );
    assert!(result.is_err());
    assert!(child.try_wait().unwrap().is_some());
}

#[cfg(unix)]
#[test]
fn relay_preserves_detailed_terminal_failure_on_nonzero_exit() {
    let work = tempfile::tempdir().unwrap();
    let progress = work.path().join("progress.jsonl");
    std::fs::write(
        &progress,
        serde_json::to_string(&HelperProgress {
            phase: HelperPhase::Failed,
            error: Some("reviewed target disconnected".into()),
            ..HelperProgress::default()
        })
        .unwrap()
            + "\n",
    )
    .unwrap();
    let mut child = Command::new("sh").args(["-c", "exit 1"]).spawn().unwrap();
    let mut events = Vec::new();
    relay_helper_progress(
        &mut child,
        &progress,
        &work.path().join("cancel"),
        &|| false,
        &mut |event| events.push(event),
    )
    .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(
        events[0].error.as_deref(),
        Some("reviewed target disconnected")
    );
}

#[test]
fn progress_output_never_overwrites_existing_file() {
    let work = tempfile::tempdir().unwrap();
    let output = work.path().join("progress.jsonl");
    std::fs::write(&output, b"keep").unwrap();
    assert_eq!(
        create_progress_output(&output).unwrap_err().kind(),
        io::ErrorKind::AlreadyExists
    );
    assert_eq!(std::fs::read(output).unwrap(), b"keep");
}

#[test]
fn cancellation_marker_must_stay_in_private_job_directory() {
    let work = tempfile::Builder::new()
        .prefix("tilo-imager-write-")
        .tempdir()
        .unwrap();
    let outside = tempfile::tempdir().unwrap();
    let request = work.path().join("request.json");
    std::fs::write(&request, b"{}").unwrap();
    assert!(
        validate_helper_paths(
            &request,
            &work.path().join("progress.jsonl"),
            &outside.path().join("cancel")
        )
        .is_err()
    );
}
