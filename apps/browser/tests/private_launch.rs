#[test]
fn private_window_flag_is_valid_in_the_live_launch_parser() {
    let fixture = tempfile::tempdir().unwrap();
    let output = fixture.path().join("settings.png");
    let result = std::process::Command::new(tilo_test_env::package_binary!("browser"))
        .args(["--private", "--scenario", "settings", "--shots"])
        .arg(&output)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.is_file());
}
