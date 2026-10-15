//! Exercise activation through the production executable, not a listener helper.
#![cfg(unix)]

#[test]
fn production_gateway_serves_prequeued_request_without_rebinding() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/socket_activation.py"
        ))
        .arg(env!("CARGO_BIN_EXE_tilo-model-gateway"))
        .output()
        .expect("Python must be installed to exercise production socket activation");
    assert!(
        output.status.success(),
        "production socket activation failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
