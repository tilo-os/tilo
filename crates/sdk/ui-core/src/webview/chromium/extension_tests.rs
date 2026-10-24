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

#[test]
fn the_engine_is_told_each_extensions_commands_and_which_one_to_run() {
    use crate::webview::ExtensionCommandInfo;
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let command = |name: &str, shortcut: &str| ExtensionCommandInfo {
        name: name.into(),
        description: "Mark the page".into(),
        shortcut: shortcut.into(),
    };
    let commands = WebViewCommand::ExtensionCommands {
        id: id.into(),
        commands: vec![
            command("mark", "Ctrl+Shift+Y"),
            command("_execute_action", ""),
        ],
    };
    assert!(command_within_limits(&commands));
    assert_eq!(
        encode_command(commands),
        Some(json!({"op":"extension_commands","id":id,"commands":[
            {"name":"mark","description":"Mark the page","shortcut":"Ctrl+Shift+Y"},
            {"name":"_execute_action","description":"Mark the page","shortcut":""},
        ]}))
    );
    let run = WebViewCommand::ExtensionCommand {
        id: id.into(),
        name: "mark".into(),
    };
    assert!(command_within_limits(&run));
    assert_eq!(
        encode_command(run),
        Some(json!({"op":"extension_command","id":id,"name":"mark"}))
    );
    for bad in [
        WebViewCommand::ExtensionCommand {
            id: id.into(),
            name: String::new(),
        },
        WebViewCommand::ExtensionCommand {
            id: "short".into(),
            name: "mark".into(),
        },
        WebViewCommand::ExtensionCommands {
            id: id.into(),
            commands: vec![command("mark", ""); 65],
        },
        WebViewCommand::ExtensionCommands {
            id: id.into(),
            commands: vec![command(&"m".repeat(129), "")],
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }
}
