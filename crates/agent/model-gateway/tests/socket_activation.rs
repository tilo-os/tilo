//! Exercise activation through the production executable, not a listener helper.
#![cfg(unix)]

#[test]
fn production_gateway_serves_prequeued_request_without_rebinding() {
    // Nextest relocates executables when running an archived test build.
    let binary = std::env::var_os("NEXTEST_BIN_EXE_tilo-model-gateway")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_tilo-model-gateway").into());
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/socket_activation.py"
        ))
        .arg(binary)
        .output()
        .expect("Python must be installed to exercise production socket activation");
    assert!(
        output.status.success(),
        "production socket activation failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
