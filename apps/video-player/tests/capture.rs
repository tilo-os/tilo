//! Explicit file captures must prove playback rather than save a placeholder.

#![allow(
    clippy::expect_used,
    reason = "test prerequisites must fail explicitly"
)]

#[test]
fn an_unreadable_video_cannot_produce_a_successful_capture() {
    let output_path = std::env::temp_dir().join(format!(
        "tilo-video-invalid-capture-{}.png",
        std::process::id()
    ));
    let missing_path = output_path.with_extension("missing-video");
    assert!(!output_path.exists());
    assert!(!missing_path.exists());
    let mut command = std::process::Command::new(tilo_test_env::package_binary!("video-player"));
    command.arg("--shots").arg(&output_path).arg(missing_path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x0800_0000);
    }
    let output = command.output().expect("run the production video capture");
    assert!(!output.status.success());
    assert!(!output_path.exists(), "an undecoded video is not a capture");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("did not settle"),
        "the capture refuses to show a video that never decoded: {error}"
    );
}
