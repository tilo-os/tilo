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

#[test]
fn a_page_menu_arrives_with_check_marks_separators_and_submenus() {
    use crate::webview::ContextMenuItem;
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    let menu =
        |items: Value| json!({"event":"context_menu","id":3,"x":10,"y":20,"link":"","items":items});
    assert_eq!(
        event(menu(json!([
            {"id":100,"label":"Back"},
            {"separator":true},
            {"id":27000,"label":"Remember the page","checked":true},
            {"id":27001,"label":"Not here","enabled":false},
            {"id":27002,"label":"Translate","items":[{"id":27003,"label":"To German","checked":false}]}
        ]))),
        Some(WebViewEvent::ContextMenu {
            id: 3,
            x: 10.0,
            y: 20.0,
            link: String::new(),
            items: vec![
                ContextMenuItem::Command {
                    id: 100,
                    label: "Back".into(),
                    checked: None,
                    enabled: true
                },
                ContextMenuItem::Separator,
                ContextMenuItem::Command {
                    id: 27000,
                    label: "Remember the page".into(),
                    checked: Some(true),
                    enabled: true
                },
                ContextMenuItem::Command {
                    id: 27001,
                    label: "Not here".into(),
                    checked: None,
                    enabled: false
                },
                ContextMenuItem::Submenu {
                    label: "Translate".into(),
                    enabled: true,
                    items: vec![ContextMenuItem::Command {
                        id: 27003,
                        label: "To German".into(),
                        checked: Some(false),
                        enabled: true
                    }],
                },
            ],
        })
    );
    // Five levels, more than 24 rows at a level or 200 in all, or a check
    // mark that is not one: refused whole.
    let mut deep = json!([{"id":1,"label":"Five"}]);
    for level in 0..4 {
        deep = json!([{"id":level + 2,"label":"Deeper","items":deep}]);
    }
    let wide: Vec<Value> = (1..=25).map(|id| json!({"id":id,"label":"Row"})).collect();
    let many: Vec<Value> = (1..=9)
        .map(|id| {
            let rows: Vec<Value> = (1..=23)
                .map(|row| json!({"id":row,"label":"Row"}))
                .collect();
            json!({"id":id,"label":"Rows","items":rows})
        })
        .collect();
    for items in [
        deep,
        Value::Array(wide),
        Value::Array(many),
        json!([{"id":1,"label":"Row","checked":"yes"}]),
        json!([{"separator":false}]),
        json!([{"id":0,"label":"Row"}]),
    ] {
        assert_eq!(event(menu(items.clone())), None, "{items}");
    }
}

#[test]
fn an_extensions_button_menu_arrives_and_a_choice_names_its_item() {
    use crate::webview::ExtensionMenuItem;
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(json!({"event":"extension_menu","tab":1,"id":id,"items":[
            {"type":"checkbox","key":"\"dark\"","label":"Dark pages","enabled":true,"checked":true},
            {"type":"separator"},
            {"type":"normal","key":"\"lists\"","label":"Lists","enabled":true,"items":[
                {"type":"radio","key":"1","label":"Easy","enabled":false,"checked":false}
            ]}
        ]})),
        Some(WebViewEvent::ExtensionMenu {
            id: id.into(),
            items: vec![
                ExtensionMenuItem::Command {
                    key: "\"dark\"".into(),
                    label: "Dark pages".into(),
                    checked: Some(true),
                    enabled: true
                },
                ExtensionMenuItem::Separator,
                ExtensionMenuItem::Submenu {
                    label: "Lists".into(),
                    enabled: true,
                    items: vec![ExtensionMenuItem::Command {
                        key: "1".into(),
                        label: "Easy".into(),
                        checked: Some(false),
                        enabled: false
                    }],
                },
            ],
        })
    );
    assert_eq!(
        event(json!({"event":"extension_menu","id":id,"items":[]})),
        Some(WebViewEvent::ExtensionMenu {
            id: id.into(),
            items: Vec::new()
        })
    );
    for items in [
        json!([{"type":"checkbox","key":"1","label":"No state","enabled":true}]),
        json!([{"type":"radio","key":"1","label":"Under","enabled":true,"checked":true,"items":[]}]),
        json!([{"type":"normal","key":"","label":"No key","enabled":true}]),
        json!([{"type":"other","key":"1","label":"Odd","enabled":true}]),
    ] {
        assert_eq!(
            event(json!({"event":"extension_menu","id":id,"items":items.clone()})),
            None,
            "{items}"
        );
    }
    assert_eq!(
        event(json!({"event":"extension_menu","id":"short","items":[]})),
        None
    );

    let choice = WebViewCommand::ExtensionMenuChoice {
        id: id.into(),
        key: "\"dark\"".into(),
    };
    assert!(command_within_limits(&choice));
    assert_eq!(
        encode_command(choice),
        Some(json!({"op":"extension_menu_action","id":id,"key":"\"dark\""}))
    );
    for bad in [
        WebViewCommand::ExtensionMenuChoice {
            id: id.into(),
            key: String::new(),
        },
        WebViewCommand::ExtensionMenuChoice {
            id: id.into(),
            key: "k".repeat(1025),
        },
        WebViewCommand::ExtensionMenuChoice {
            id: "short".into(),
            key: "1".into(),
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }
}
