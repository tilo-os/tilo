use super::*;

#[test]
fn the_engine_is_told_what_extensions_may_do_with_its_tabs() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let access = WebViewCommand::ExtensionAccess {
        id: id.into(),
        permissions: vec!["tabs".into()],
        hosts: vec!["https://*/*".into()],
        scriptable: vec!["<all_urls>".into()],
        token: Some("f".repeat(48)),
        commands: Some(json!({"_execute_action":{"suggested_key":{"default":"Ctrl+Shift+Y"}}})),
    };
    assert!(command_within_limits(&access));
    assert_eq!(
        encode_command(access),
        Some(
            json!({"op":"extension_access","id":id,"permissions":["tabs"],"hosts":["https://*/*"],
                "scriptable":["<all_urls>"],"token":"f".repeat(48),
                "commands":{"_execute_action":{"suggested_key":{"default":"Ctrl+Shift+Y"}}}})
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
            scriptable: Vec::new(),
            token: Some("f".into()),
            commands: None,
        },
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: vec![String::new(); 513],
            hosts: Vec::new(),
            scriptable: Vec::new(),
            token: None,
            commands: None,
        },
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: Vec::new(),
            hosts: Vec::new(),
            scriptable: vec!["x".repeat(2049)],
            token: None,
            commands: None,
        },
        // Commands that are no manifest's object, or past any manifest's size.
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: Vec::new(),
            hosts: Vec::new(),
            scriptable: Vec::new(),
            token: None,
            commands: Some(json!(["_execute_action"])),
        },
        WebViewCommand::ExtensionAccess {
            id: id.into(),
            permissions: Vec::new(),
            hosts: Vec::new(),
            scriptable: Vec::new(),
            token: None,
            commands: Some(json!({"long": {"description": "x".repeat(70 * 1024)}})),
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }
}

#[test]
fn an_extensions_button_state_arrives_for_one_tab_or_every_tab() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(
            json!({"event":"extension_action","tab":2,"id":id,"global":false,
            "change":{"badge":"12","title":null,"enabled":false,
                "popup":format!("chrome-extension://{id}/popup.html"),"icon":{"path":"/on.png"}}})
        ),
        Some(WebViewEvent::ExtensionAction {
            id: id.into(),
            global: false,
            change: crate::webview::ExtensionActionChange {
                badge: Some(Some("12".into())),
                title: Some(None),
                popup: Some(Some(format!("chrome-extension://{id}/popup.html"))),
                enabled: Some(Some(false)),
                icon: Some(Some(crate::webview::ExtensionActionIcon::Path(
                    "/on.png".into()
                ))),
            },
        })
    );
    // Pixels an extension drew: exactly width * height RGBA.
    assert_eq!(
        event(
            json!({"event":"extension_action","tab":2,"id":id,"global":true,
            "change":{"icon":{"width":1,"height":1,"data":"AQIDBA=="},"popup":""}})
        ),
        Some(WebViewEvent::ExtensionAction {
            id: id.into(),
            global: true,
            change: crate::webview::ExtensionActionChange {
                popup: Some(Some(String::new())),
                icon: Some(Some(crate::webview::ExtensionActionIcon::Pixels {
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 4],
                })),
                ..crate::webview::ExtensionActionChange::default()
            },
        })
    );
    assert_eq!(
        event(json!({"event":"extension_action_reset","tab":2})),
        Some(WebViewEvent::ExtensionActionReset)
    );
    assert_eq!(
        event(json!({"event":"extension_execute_action","tab":2,"id":id})),
        Some(WebViewEvent::ExtensionExecuteAction { id: id.into() })
    );
    assert_eq!(
        event(json!({"event":"extension_open_popup","tab":2,"id":id,
            "url":format!("chrome-extension://{id}/popup.html")})),
        Some(WebViewEvent::ExtensionOpenPopup {
            id: id.into(),
            url: format!("chrome-extension://{id}/popup.html"),
        })
    );
    // Another extension's page, an unknown field, a misfit icon: refused.
    let other = "bbcdefghijklmnopabcdefghijklmnop";
    for bad in [
        json!({"event":"extension_open_popup","id":id,"url":format!("chrome-extension://{other}/p.html")}),
        json!({"event":"extension_open_popup","id":id,"url":"https://example.test/"}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"popup":format!("chrome-extension://{other}/p.html")}}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"colour":"red"}}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"badge":7}}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"icon":{"width":2,"height":1,"data":"AQIDBA=="}}}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"icon":{"width":200,"height":200,"data":""}}}),
        json!({"event":"extension_action","id":id,"global":true,"change":{"icon":{"path":"on.png"}}}),
        json!({"event":"extension_action","id":"short","global":true,"change":{}}),
        json!({"event":"extension_execute_action","id":"short"}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
}
