use super::*;
use std::process::{Command, Stdio};
use tilo_test_env::SpawnUnderEnvLock as _;

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
        .spawn_locked()
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

tilo_test_env::scenario! {
    fn capture_ends_only_on_its_own_release_and_reaps_its_child;

    phase cancellation_waits_for_matching_release;
    {
        let mut capture = helper(&released(OPERATION));
        capture.cancel_and_wait().unwrap();
        assert!(capture.poll().unwrap().is_none());
    }

    phase clean_exit_without_release_is_not_success;
    {
        let mut capture = helper("");
        assert_eq!(
            capture.cancel_and_wait().unwrap_err().to_string(),
            "capture release was not acknowledged"
        );
    }

    phase substituted_operation_cannot_acknowledge_release;
    {
        let mut capture = helper(&released("1f273186-e49a-4f14-8507-e13c6c16925a"));
        assert_eq!(
            capture.cancel_and_wait().unwrap_err().to_string(),
            "capture status operation changed"
        );
    }

    phase oversized_and_unterminated_frames_fail_closed;
    {
        for frame in ["x".repeat(2049), released(OPERATION).trim_end().into()] {
            let mut capture = helper(&frame);
            assert_eq!(
                capture.cancel_and_wait().unwrap_err().to_string(),
                "capture status frame exceeds bound"
            );
        }
    }

    phase drop_reaps_before_releasing_request_directory;
    {
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
            .spawn_locked()
            .unwrap();
        let capture = Capture::attach(child, request, OPERATION).unwrap();
        drop(capture);
        assert_eq!(std::fs::read(marker).unwrap(), b"done");
        assert!(!directory.exists());
    }
}
