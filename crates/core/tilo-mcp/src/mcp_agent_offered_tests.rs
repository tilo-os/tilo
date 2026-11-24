//! An action the app declares and the caller is not offered is refused as that, naming the
//! action and the caller; a name the app does not declare is no action of it. Only a caller
//! whose own catalogue lists the app learns that an action exists.
use super::*;

/// What the installed Notes declares (its package's surface has `edit-text`) and this Notes
/// offers nobody: every surface the call is made through says it exists, and to whom it is
/// not offered.
const DECLARED: &str = "edit-text";

#[tokio::test]
async fn a_declared_action_is_not_offered_and_an_undeclared_one_does_not_exist() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let refused = "not_offered: so.tilo.notes#edit-text isn't offered to claude-code here";
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": format!("notes#{DECLARED}")}),
    )
    .await;
    assert!(failed && text == refused, "{text}");
    let (text, failed) = tool(
        &server,
        "tilo_ls",
        json!({"path": format!("notes#{DECLARED}")}),
    )
    .await;
    assert!(
        failed && text == refused,
        "listing it says the same: {text}"
    );
    // Typed, the refusal carries its own code: an SDK raises it as that.
    let typed = server
        .handle(json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
            "params":{"name":"tilo_do","arguments":{"path": format!("notes#{DECLARED}")},
                      "_meta":{"so.tilo.mcp":{"structured":true}}}}))
        .await
        .unwrap();
    assert_eq!(
        typed["result"]["structuredContent"]["error"]["code"], "not_offered",
        "{typed}"
    );
    assert!(
        notes.invoked.lock().unwrap().is_empty(),
        "nothing ran for it"
    );

    // The person's own call names them as the one it is not offered to.
    let person = McpServer::for_person(notes.clone(), "workspace".into(), "ada");
    let (text, failed) = tool(
        &person,
        "tilo_do",
        json!({"path": format!("notes#{DECLARED}")}),
    )
    .await;
    assert!(
        failed && text == "not_offered: so.tilo.notes#edit-text isn't offered to you here",
        "{text}"
    );

    // A name Notes does not declare is no action of it.
    let (text, failed) = tool(
        &server,
        "tilo_do",
        json!({"path": "notes#not-a-real-action"}),
    )
    .await;
    assert!(
        failed
            && text.starts_with("unknown_action:")
            && text.contains("notes has no action not-a-real-action"),
        "{text}"
    );

    // A caller whose catalogue lists no Notes is told nothing more than that the action is
    // none: the same refusal as for a name Notes does not declare.
    let unlisted = Notes::new(true).installed(Installed::Nothing);
    let (text, failed) = tool(
        &agent(&unlisted),
        "tilo_do",
        json!({"path": format!("notes#{DECLARED}")}),
    )
    .await;
    assert!(
        failed
            && text.starts_with("unknown_action:")
            && text.contains(&format!("has no action {DECLARED}"))
            && !text.contains("offered to"),
        "{text}"
    );
}
