//! Exercise activation through the production executable, not a listener helper.
#![cfg(unix)]

#[test]
fn production_gateway_serves_prequeued_request_without_rebinding() {
    let binary = tilo_test_env::package_binary!("tilo-model-gateway");
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
