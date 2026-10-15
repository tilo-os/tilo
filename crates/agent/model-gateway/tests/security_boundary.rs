use std::fs;

#[test]
fn api_key_is_never_a_command_line_option() {
    let source =
        fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/src/service.rs")).unwrap();
    assert!(!source.contains("--api-key"));
    assert!(
        !source.contains("api_key: String }"),
        "control CLI must not carry a key"
    );
}
