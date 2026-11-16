//! `tilo_status` on the agent surface, over the agent tests' hosts.

use super::*;

/// `tilo_status` naming no invocation says what this session ran and how
/// each stands, the calls that named no id of their own among them, rather
/// than refusing the call for a missing field.
#[test]
fn a_status_naming_no_invocation_lists_the_sessions_invocations() {
    let host = Notes::new(true);
    run(async {
        let server = agent(&host);
        let (text, failed) = tool(&server, "tilo_status", json!({})).await;
        assert!(!failed, "{text}");
        assert!(text.contains("ran nothing yet"), "{text}");
        let (text, failed) = tool(
            &server,
            "tilo_do",
            json!({"path": "notes/Todo/body#append-paragraph", "args": {"text": "Buy milk"}}),
        )
        .await;
        assert!(!failed, "{text}");
        let (text, failed) = tool(&server, "tilo_status", json!({})).await;
        assert!(!failed, "{text}");
        let line = text
            .lines()
            .find(|line| line.contains("notes/Todo/body#append-paragraph"))
            .unwrap_or_else(|| panic!("the call is listed: {text}"));
        let id = line.split_whitespace().next().unwrap();
        assert!(line.ends_with("succeeded"), "{line}");
        // The id it lists is one `tilo_status` answers for.
        let (one, failed) = tool(&server, "tilo_status", json!({"invocation_id": id})).await;
        assert!(!failed, "{one}");
        let (refused, failed) = tool(&server, "tilo_status", json!({"cancel": true})).await;
        assert!(failed && refused.contains("invocation_id"), "{refused}");
    });
}
