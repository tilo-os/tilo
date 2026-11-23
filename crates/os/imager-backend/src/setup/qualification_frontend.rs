//! Test-only process boundary to the UI-backed qualification fixture.
//! Native tests never link a renderer; the separately built app test executable
//! exercises the actual journey and returns descriptive data, not root grants.
use std::io::Write;

pub fn call(request: serde_json::Value) -> serde_json::Value {
    let executable = std::env::var_os("TILO_IMAGER_FRONTEND_TESTS")
        .expect("qualification requires the matching frontend test executable");
    let mut child = std::process::Command::new(executable)
        .args([
            "setup::qualification_frontend::native_fixture",
            "--exact",
            "--nocapture",
        ])
        .env("TILO_IMAGER_FRONTEND_FIXTURE", "1")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(&serde_json::to_vec(&request).unwrap())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "frontend qualification failed: {}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("1 passed; 0 failed; 0 ignored"));
    let response = stdout
        .lines()
        .find_map(|line| line.strip_prefix("TILO_FRONTEND_RESPONSE "))
        .expect("frontend fixture did not return a response");
    serde_json::from_str(response).unwrap()
}
