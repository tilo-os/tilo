use super::*;
use crate::judge::Case;

#[test]
fn the_profile_defaults_to_the_small_model_and_reads_its_key_from_the_store() {
    let default = profile("moderation", None);
    assert_eq!(default.default_model.as_deref(), Some(crate::DEFAULT_MODEL));
    assert_eq!(default.endpoint, "https://api.anthropic.com");
    let store = SecretStore::memory(SECRET_APP);
    assert!(matches!(
        AnthropicJudge::from_profile(&default, &store),
        Err(JudgeError::Unconfigured(_))
    ));
    store_key(&store, &default, b"sk-test\n").unwrap();
    let judge = AnthropicJudge::from_profile(&default, &store).unwrap();
    assert_eq!(judge.model(), crate::DEFAULT_MODEL);
    assert!(
        !format!("{judge:?}").contains("sk-test"),
        "the key is never printed"
    );
    let other = profile("moderation-2", Some("claude-sonnet-5-5"));
    store_key(&store, &other, b"sk-test").unwrap();
    assert_eq!(
        AnthropicJudge::from_profile(&other, &store)
            .unwrap()
            .model(),
        "claude-sonnet-5-5"
    );

    let body = judge.body(&Case {
        person: "p".into(),
        reports: Vec::new(),
        activity: Vec::new(),
        earlier: Vec::new(),
    });
    assert_eq!(body["model"], crate::DEFAULT_MODEL);
    assert_eq!(body["system"], INSTRUCTIONS);
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert_eq!(body["messages"][0]["role"], "user");
}

#[test]
fn a_mounted_key_is_kept_in_the_store_and_never_shown() {
    let dir = tempfile::tempdir().unwrap();
    let store = SecretStore::memory(SECRET_APP);
    let file = dir.path().join("moderation-model-key");
    assert!(matches!(
        import_mounted_key(&store, &file, None),
        Err(JudgeError::Unconfigured(_))
    ));
    std::fs::write(&file, "  \n").unwrap();
    assert!(import_mounted_key(&store, &file, None).is_err());
    std::fs::write(&file, "sk-mounted\n").unwrap();
    let judge = import_mounted_key(&store, &file, Some("claude-sonnet-5-5")).unwrap();
    assert_eq!(judge.model(), "claude-sonnet-5-5");
    assert!(!format!("{judge:?}").contains("sk-mounted"));
    let kept = store
        .item(KEY_PURPOSE, HOST_PROFILE)
        .get_blocking()
        .unwrap()
        .unwrap();
    assert_eq!(kept.expose(), b"sk-mounted");
}

#[test]
fn an_answer_is_its_text_unless_refused_or_cut_off() {
    let answer = serde_json::json!({
        "stop_reason": "end_turn",
        "content": [
            {"type": "thinking", "thinking": ""},
            {"type": "text", "text": "{\"outcome\":\"no-action\"}"}
        ]
    });
    assert_eq!(answer_text(&answer).unwrap(), "{\"outcome\":\"no-action\"}");
    for stop in ["refusal", "max_tokens"] {
        let refused = serde_json::json!({"stop_reason": stop, "content": []});
        assert!(answer_text(&refused).is_err());
    }
}
