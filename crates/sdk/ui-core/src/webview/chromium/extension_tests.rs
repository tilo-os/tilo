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

#[test]
fn an_extensions_notification_arrives_and_a_choice_goes_back() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(
            json!({"event":"extension_notification","tab":2,"id":id,"notification":"copy",
            "title":"Copying","message":"Two files","context":"","items":[{"title":"a","message":"b"}],
            "buttons":["Pause","Stop"],"progress":40,"require_interaction":false,"silent":false})
        ),
        Some(WebViewEvent::ExtensionNotification {
            id: id.into(),
            notification: "copy".into(),
            notice: Some(crate::webview::ExtensionNotice {
                title: "Copying".into(),
                message: "Two files".into(),
                context: String::new(),
                items: vec![("a".into(), "b".into())],
                buttons: vec!["Pause".into(), "Stop".into()],
                progress: Some(40),
                silent: false,
            }),
        })
    );
    // A quiet one (asked so, or a change to one shown) says so.
    let quiet = event(
        json!({"event":"extension_notification","tab":2,"id":id,"notification":"copy",
        "title":"Copying","message":"Two files","context":"","items":[],"buttons":[],
        "progress":80,"require_interaction":false,"silent":true}),
    );
    assert!(
        matches!(&quiet, Some(WebViewEvent::ExtensionNotification { notice: Some(notice), .. }) if notice.silent),
        "{quiet:?}"
    );
    assert_eq!(
        event(
            json!({"event":"extension_notification","tab":2,"id":id,"notification":"copy","cleared":true})
        ),
        Some(WebViewEvent::ExtensionNotification {
            id: id.into(),
            notification: "copy".into(),
            notice: None,
        })
    );
    // Past the host's bounds, or not an extension's: refused.
    let shown = |extra: Value| {
        let mut value = json!({"event":"extension_notification","id":id,"notification":"n",
            "title":"T","message":"M","context":"","items":[],"buttons":[]});
        for (key, field) in extra.as_object().unwrap() {
            value[key] = field.clone();
        }
        value
    };
    assert!(event(shown(json!({}))).is_some());
    for bad in [
        shown(json!({"progress":101})),
        shown(json!({"buttons":["a","b","c"]})),
        shown(json!({"title":"x".repeat(4097)})),
        shown(json!({"notification":"n".repeat(501)})),
        shown(json!({"id":"short"})),
        shown(json!({"message":null})),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }

    let chosen = |button| WebViewCommand::ExtensionNotificationChosen {
        id: id.into(),
        notification: "copy".into(),
        button,
    };
    assert!(command_within_limits(&chosen(Some(1))));
    assert_eq!(
        encode_command(chosen(Some(1))),
        Some(
            json!({"op":"extension_notification_chosen","id":id,"notification":"copy","button":1})
        )
    );
    assert_eq!(
        encode_command(chosen(None)),
        Some(json!({"op":"extension_notification_chosen","id":id,"notification":"copy"}))
    );
    assert!(!command_within_limits(&chosen(Some(2))));
}

#[test]
fn an_extensions_request_for_more_access_reaches_the_browser_and_its_answer_the_engine() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let set = |permissions: &[&str], origins: &[&str]| crate::webview::ExtensionPermissionSet {
        permissions: permissions.iter().map(|&entry| entry.to_owned()).collect(),
        origins: origins.iter().map(|&entry| entry.to_owned()).collect(),
    };
    let grants = WebViewCommand::ExtensionGrants {
        id: id.into(),
        optional: set(&["tabs"], &["https://*.example.org/*"]),
        granted: set(&["tabs"], &[]),
    };
    assert!(command_within_limits(&grants));
    assert_eq!(
        encode_command(grants),
        Some(json!({"op":"extension_grants","id":id,
            "optional":{"permissions":["tabs"],"hosts":["https://*.example.org/*"]},
            "granted":{"permissions":["tabs"],"hosts":[]}}))
    );
    let decision = WebViewCommand::ExtensionPermissionDecision {
        request: 3,
        allow: true,
    };
    assert!(command_within_limits(&decision));
    assert_eq!(
        encode_command(decision),
        Some(json!({"op":"extension_permission_decision","request":3,"allow":true}))
    );
    for bad in [
        WebViewCommand::ExtensionPermissionDecision {
            request: 0,
            allow: true,
        },
        WebViewCommand::ExtensionGrants {
            id: "short".into(),
            optional: set(&[], &[]),
            granted: set(&[], &[]),
        },
        WebViewCommand::ExtensionGrants {
            id: id.into(),
            optional: crate::webview::ExtensionPermissionSet {
                permissions: vec![String::new(); 513],
                origins: Vec::new(),
            },
            granted: set(&[], &[]),
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }

    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(
            json!({"event":"extension_permission_request","tab":2,"request":4,"id":id,
            "permissions":["tabs"],"origins":["http://127.0.0.1/*"]})
        ),
        Some(WebViewEvent::ExtensionPermissionRequest {
            request: 4,
            id: id.into(),
            asked: set(&["tabs"], &["http://127.0.0.1/*"]),
        })
    );
    // All it holds now, also nothing.
    assert_eq!(
        event(
            json!({"event":"extension_permissions","tab":2,"id":id,"permissions":[],"origins":[]})
        ),
        Some(WebViewEvent::ExtensionPermissions {
            id: id.into(),
            granted: set(&[], &[]),
        })
    );
    // A request for nothing, without its number, from no extension, or of
    // another shape: refused.
    for bad in [
        json!({"event":"extension_permission_request","request":4,"id":id,"permissions":[],"origins":[]}),
        json!({"event":"extension_permission_request","id":id,"permissions":["tabs"],"origins":[]}),
        json!({"event":"extension_permission_request","request":0,"id":id,"permissions":["tabs"],"origins":[]}),
        json!({"event":"extension_permission_request","request":4,"id":"short","permissions":["tabs"],"origins":[]}),
        json!({"event":"extension_permissions","id":id,"permissions":"tabs","origins":[]}),
        json!({"event":"extension_permissions","id":id,"permissions":[7],"origins":[]}),
        json!({"event":"extension_permissions","id":id,"permissions":[]}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
}

#[test]
fn an_extensions_request_to_open_a_download_names_the_download_and_how() {
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(json!({"event":"extension_download_open","tab":2,"id":7,"reveal":false})),
        Some(WebViewEvent::ExtensionDownloadOpen {
            id: 7,
            reveal: false
        })
    );
    assert_eq!(
        event(json!({"event":"extension_download_open","tab":2,"id":7,"reveal":true})),
        Some(WebViewEvent::ExtensionDownloadOpen {
            id: 7,
            reveal: true
        })
    );
    for bad in [
        json!({"event":"extension_download_open","tab":2,"id":0,"reveal":false}),
        json!({"event":"extension_download_open","tab":2,"id":"7","reveal":false}),
        json!({"event":"extension_download_open","tab":2,"id":7}),
        json!({"event":"extension_download_open","tab":2,"id":7,"reveal":"yes"}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
}

#[test]
fn an_extension_asks_the_person_to_sign_in_on_a_web_page() {
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    let page = "https://accounts.example/authorize?client_id=1&redirect_uri=x";
    assert_eq!(
        event(
            json!({"event":"extension_sign_in","tab":2,"id":id,"url":page,"interactive":true,"flow":3})
        ),
        Some(WebViewEvent::ExtensionSignIn {
            id: id.into(),
            url: page.into(),
            interactive: true,
            flow: 3,
        })
    );
    // Only a web page, of a bounded length, from an extension's id, for a
    // sign-in the engine numbered.
    let sign_in = |url: Value| json!({"event":"extension_sign_in","id":id,"url":url,"interactive":false,"flow":1});
    for bad in [
        sign_in(json!(format!("chrome-extension://{id}/p.html"))),
        sign_in(json!("javascript:alert(1)")),
        sign_in(json!("file:///etc/passwd")),
        sign_in(json!(format!("https://example.test/{}", "a".repeat(8192)))),
        sign_in(Value::Null),
        json!({"event":"extension_sign_in","id":"short","url":page,"interactive":true,"flow":1}),
        json!({"event":"extension_sign_in","id":id,"url":page,"flow":1}),
        json!({"event":"extension_sign_in","id":id,"url":page,"interactive":true,"flow":0}),
        json!({"event":"extension_sign_in","id":id,"url":page,"interactive":true,"flow":1u64 << 31}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
    // The tab the Browser shows it in is marked as that sign-in's.
    let panel = WebViewCommand::ExtensionSignInPanel {
        id: id.into(),
        flow: 3,
    };
    assert!(command_within_limits(&panel));
    assert_eq!(
        encode_command(panel),
        Some(json!({"op":"extension_sign_in_panel","id":id,"flow":3}))
    );
    for bad in [
        WebViewCommand::ExtensionSignInPanel {
            id: "ABCDEFGHIJKLMNOPABCDEFGHIJKLMNOP".into(),
            flow: 3,
        },
        WebViewCommand::ExtensionSignInPanel {
            id: id.into(),
            flow: 0,
        },
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }
}

#[test]
fn what_follows_an_extensions_keyword_goes_to_it_and_its_suggestions_come_back() {
    use crate::webview::{OmniboxInput, OmniboxSuggestion};
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let omnibox = |input: OmniboxInput| WebViewCommand::ExtensionOmnibox {
        id: id.into(),
        input,
    };
    for (input, wire) in [
        (
            OmniboxInput::Started,
            json!({"op":"extension_omnibox","id":id,"input":"started"}),
        ),
        (
            OmniboxInput::Changed {
                text: "cat".into(),
                request: 3,
            },
            json!({"op":"extension_omnibox","id":id,"input":"changed","text":"cat","request":3}),
        ),
        (
            OmniboxInput::Entered {
                text: "cat one".into(),
            },
            json!({"op":"extension_omnibox","id":id,"input":"entered","text":"cat one"}),
        ),
        (
            OmniboxInput::Cancelled,
            json!({"op":"extension_omnibox","id":id,"input":"cancelled"}),
        ),
    ] {
        let command = omnibox(input);
        assert!(command_within_limits(&command), "{command:?}");
        assert_eq!(encode_command(command), Some(wire));
    }
    // Not an extension, more than the host reads, a request past its range.
    for bad in [
        WebViewCommand::ExtensionOmnibox {
            id: "short".into(),
            input: OmniboxInput::Started,
        },
        omnibox(OmniboxInput::Entered {
            text: "x".repeat(2049),
        }),
        omnibox(OmniboxInput::Changed {
            text: String::new(),
            request: u32::MAX,
        }),
    ] {
        assert!(!command_within_limits(&bad), "{bad:?}");
    }

    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    assert_eq!(
        event(
            json!({"event":"extension_omnibox_suggestions","tab":1,"id":id,"request":3,
            "suggestions":[{"content":"cat one","description":"First <match>cat</match>"}]})
        ),
        Some(WebViewEvent::ExtensionOmniboxSuggestions {
            id: id.into(),
            request: 3,
            suggestions: vec![OmniboxSuggestion {
                content: "cat one".into(),
                description: "First <match>cat</match>".into(),
            }],
        })
    );
    assert_eq!(
        event(
            json!({"event":"extension_omnibox_default","tab":1,"id":id,"description":"Look up %s"})
        ),
        Some(WebViewEvent::ExtensionOmniboxDefault {
            id: id.into(),
            description: Some("Look up %s".into()),
        })
    );
    assert_eq!(
        event(json!({"event":"extension_omnibox_default","tab":1,"id":id,"description":null})),
        Some(WebViewEvent::ExtensionOmniboxDefault {
            id: id.into(),
            description: None,
        })
    );
    let many: Vec<Value> = (0..17)
        .map(|n| json!({"content":n.to_string(),"description":"d"}))
        .collect();
    for bad in [
        json!({"event":"extension_omnibox_suggestions","id":"short","request":1,"suggestions":[]}),
        json!({"event":"extension_omnibox_suggestions","id":id,"request":-1,"suggestions":[]}),
        json!({"event":"extension_omnibox_suggestions","id":id,"request":1,"suggestions":many}),
        json!({"event":"extension_omnibox_suggestions","id":id,"request":1,
            "suggestions":[{"content":"x".repeat(2049),"description":"d"}]}),
        json!({"event":"extension_omnibox_suggestions","id":id,"request":1,"suggestions":[{"content":"c"}]}),
        json!({"event":"extension_omnibox_default","id":id,"description":7}),
        json!({"event":"extension_omnibox_default","id":id}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
}

#[test]
fn an_extensions_side_panel_opens_closes_and_follows_its_options() {
    use crate::webview::ExtensionSidePanelChange as Change;
    let id = "abcdefghijklmnopabcdefghijklmnop";
    let page = format!("chrome-extension://{id}/panel.html");
    let event = |value: Value| {
        validate_event(&value).ok()?;
        decode_event(&value)
    };
    let side_panel = |change| {
        Some(WebViewEvent::ExtensionSidePanel {
            id: id.into(),
            change,
        })
    };
    assert_eq!(
        event(json!({"event":"extension_side_panel","tab":2,"id":id,"mode":"open","url":page})),
        side_panel(Change::Open(page.clone()))
    );
    assert_eq!(
        event(json!({"event":"extension_side_panel","tab":2,"id":id,"mode":"toggle","url":page})),
        side_panel(Change::Toggle(page.clone()))
    );
    assert_eq!(
        event(json!({"event":"extension_side_panel","tab":2,"id":id,"mode":"options","url":page})),
        side_panel(Change::Options(Some(page.clone())))
    );
    assert_eq!(
        event(json!({"event":"extension_side_panel","tab":2,"id":id,"mode":"options","url":null})),
        side_panel(Change::Options(None))
    );
    assert_eq!(
        event(json!({"event":"extension_side_panel","tab":2,"id":id,"mode":"close","url":null})),
        side_panel(Change::Close)
    );
    // Another extension's page, a web page, a page to open missing, an
    // unknown mode: refused.
    let other = "bbcdefghijklmnopabcdefghijklmnop";
    for bad in [
        json!({"event":"extension_side_panel","id":id,"mode":"open","url":format!("chrome-extension://{other}/p.html")}),
        json!({"event":"extension_side_panel","id":id,"mode":"options","url":"https://example.test/"}),
        json!({"event":"extension_side_panel","id":id,"mode":"open","url":null}),
        json!({"event":"extension_side_panel","id":id,"mode":"toggle"}),
        json!({"event":"extension_side_panel","id":id,"mode":"close","url":page}),
        json!({"event":"extension_side_panel","id":id,"mode":"float","url":page}),
        json!({"event":"extension_side_panel","id":"short","mode":"open","url":page}),
    ] {
        assert_eq!(event(bad.clone()), None, "{bad}");
    }
    assert_eq!(
        encode_command(WebViewCommand::ExtensionOpenSidePanel { id: id.into() }),
        Some(json!({"op":"extension_side_panel","id":id}))
    );
    assert!(!command_within_limits(
        &WebViewCommand::ExtensionOpenSidePanel { id: "short".into() }
    ));
}
