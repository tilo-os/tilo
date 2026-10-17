use super::*;
use std::process::{Command, Stdio};

const OPERATION: &str = "0f273186-e49a-4f14-8507-e13c6c16925a";

fn helper(frame: &str) -> Capture {
    let child = Command::new("sh")
        .args([
            "-c",
            "read -r control; printf '%s' \"$1\"",
            "capture-fixture",
            frame,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn capture fixture");
    Capture::attach(
        child,
        tempfile::tempdir().expect("create capture request directory"),
        OPERATION,
    )
    .expect("attach capture fixture")
}

fn released(operation: &str) -> String {
    format!("{{\"operation\":\"{operation}\",\"state\":\"released\",\"expires_unix\":null}}\n")
}

#[test]
fn cancellation_waits_for_matching_release() {
    let mut capture = helper(&released(OPERATION));
    capture.cancel_and_wait().unwrap();
    assert!(capture.poll().unwrap().is_none());
}

#[test]
fn clean_exit_without_release_is_not_success() {
    let mut capture = helper("");
    assert_eq!(
        capture.cancel_and_wait().unwrap_err().to_string(),
        "capture release was not acknowledged"
    );
}

#[test]
fn substituted_operation_cannot_acknowledge_release() {
    let mut capture = helper(&released("1f273186-e49a-4f14-8507-e13c6c16925a"));
    assert_eq!(
        capture.cancel_and_wait().unwrap_err().to_string(),
        "capture status operation changed"
    );
}

#[test]
fn oversized_and_unterminated_frames_fail_closed() {
    for frame in ["x".repeat(2049), released(OPERATION).trim_end().into()] {
        let mut capture = helper(&frame);
        assert_eq!(
            capture.cancel_and_wait().unwrap_err().to_string(),
            "capture status frame exceeds bound"
        );
    }
}

#[test]
fn drop_reaps_before_releasing_request_directory() {
    let request = tempfile::tempdir().unwrap();
    let directory = request.path().to_path_buf();
    let evidence = tempfile::tempdir().unwrap();
    let marker = evidence.path().join("reaped");
    let child = Command::new("sh")
        .args([
            "-c",
            "read -r control; test -d \"$1\" && printf done > \"$2\"",
            "capture-fixture",
        ])
        .arg(&directory)
        .arg(&marker)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let capture = Capture::attach(child, request, OPERATION).unwrap();
    drop(capture);
    assert_eq!(std::fs::read(marker).unwrap(), b"done");
    assert!(!directory.exists());
}
