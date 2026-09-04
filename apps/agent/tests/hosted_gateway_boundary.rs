use std::fs;

#[test]
fn packaged_agent_has_no_direct_provider_or_environment_key_route() {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/src/");
    let model = fs::read_to_string(format!("{root}model.rs")).unwrap();
    let providers = fs::read_to_string(format!("{root}providers.rs")).unwrap();
    assert!(model.contains("qos_model_gateway::plan"));
    assert!(!model.contains("ANTHROPIC_API_KEY"));
    assert!(!model.contains("reqwest::"));
    assert!(!providers.contains("SecretResolver"));
}
