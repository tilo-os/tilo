use super::*;

#[test]
fn the_engine_is_told_what_extensions_may_do_with_its_tabs() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let access = WebViewCommand::ExtensionAccess {
        id: id.into(),
        permissions: vec!["tabs".into()],
        hosts: vec!["https://*/*".into()],
        token: Some("f".repeat(48)),
    };
    assert!(command_within_limits(&access));
    assert_eq!(
        encode_command(access),
        Some(
            json!({"op":"extension_access","id":id,"permissions":["tabs"],"hosts":["https://*/*"],"token":"f".repeat(48)})
        )
    );
    assert_eq!(
        encode_command(WebViewCommand::ExtensionPanel {
            action: Some(id.into())
        }),
        Some(json!({"op":"extension_panel","action":id}))
    );
    assert_eq!(
        encode_command(WebViewCommand::ExtensionPanel { action: None }),
        Some(json!({"op":"extension_panel"}))
    );
    assert_eq!(
        encode_command(WebViewCommand::ExtensionClick { id: id.into() }),
        Some(json!({"op":"extension_click","id":id}))
    );
    // Not an extension's id, or a token too short to keep a worker apart.
    for bad in [
        WebViewCommand::ExtensionClick {
            id: "ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP".into(),
        },
        WebViewCommand::ExtensionIdentity { id: "short".into() },
        WebViewCommand::ExtensionPanel {
            action: Some("qqqqqqqqqqqqqqqqqqqqqqqqqqqqqqqq".into()),
        },
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: Vec::new(),
            hosts: Vec::new(),
            token: Some("f".into()),
        },
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: vec![String::new(); 513],
            hosts: Vec::new(),
            token: None,
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }
}
