//! TOML config round-trip + validation tests.

use qos_relay::RelayConfig;

#[test]
fn default_round_trips_through_toml() {
    let original = RelayConfig::default();
    let text = original.to_toml().expect("serialize");
    let parsed = RelayConfig::parse(&text).expect("re-parse");
    assert_eq!(original, parsed);
}

#[test]
fn custom_config_parses() {
    let text = r#"
        listen = ["/ip4/0.0.0.0/tcp/9001"]
        identity_path = "/tmp/qos-relay.key"
        bootstrap = []
        topic_allowlist = ["qos.*", "extra.*"]
        rate_limit_per_topic_per_minute = 120
        log_level = "qos_relay=debug"
    "#;
    let cfg = RelayConfig::parse(text).expect("parse");
    assert_eq!(cfg.listen.len(), 1);
    assert_eq!(cfg.identity_path.to_string_lossy(), "/tmp/qos-relay.key");
    assert_eq!(cfg.topic_allowlist.len(), 2);
    assert_eq!(cfg.rate_limit_per_topic_per_minute, 120);
    assert_eq!(cfg.log_level, "qos_relay=debug");
    cfg.validate().expect("custom config valid");
}

#[test]
fn invalid_toml_returns_clear_error() {
    let text = "listen = this is not valid TOML";
    let err = RelayConfig::parse(text).expect_err("must fail");
    // toml::de::Error renders a human-readable message; just confirm
    // the failure isn't an empty string.
    assert!(!err.to_string().is_empty());
}

#[test]
fn malformed_listen_addr_rejected() {
    // The TOML parses but the value is not a multiaddr — that fails at
    // parse time because we use `Multiaddr`'s serde impl.
    let text = r#"listen = ["not-a-multiaddr"]"#;
    let err = RelayConfig::parse(text).expect_err("must fail");
    assert!(!err.to_string().is_empty());
}

#[test]
fn metrics_listen_parses_when_set() {
    let text = r#"metrics_listen = "127.0.0.1:9090""#;
    let cfg = RelayConfig::parse(text).expect("parse");
    assert!(cfg.metrics_listen.is_some());
    assert_eq!(cfg.metrics_listen.unwrap().port(), 9090);
}

#[test]
fn load_from_disk_works() {
    let dir = tempfile::tempdir().expect("tempdir");
    let p = dir.path().join("relay.toml");
    let original = RelayConfig {
        log_level: "trace".to_string(),
        ..RelayConfig::default()
    };
    std::fs::write(&p, original.to_toml().unwrap()).unwrap();

    let loaded = RelayConfig::load(&p).expect("load");
    assert_eq!(loaded, original);
}

#[test]
fn load_missing_file_surfaces_path() {
    let err = RelayConfig::load(std::path::Path::new("/no/such/qos-relay-config.toml"))
        .expect_err("must fail");
    let msg = err.to_string();
    assert!(msg.contains("/no/such/qos-relay-config.toml"), "got: {msg}");
}
