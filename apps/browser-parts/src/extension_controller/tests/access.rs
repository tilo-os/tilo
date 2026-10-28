//! What an extension may do: the access it asks for through the window,
//! and what the engine is told of it as that changes.

use super::*;

#[test]
fn an_extension_asks_for_more_access_through_the_window() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let set = |permissions: &[&str], origins: &[&str]| ExtensionPermissionSet {
        permissions: permissions.iter().map(|&entry| entry.to_owned()).collect(),
        origins: origins.iter().map(|&entry| entry.to_owned()).collect(),
    };
    let access = toolbar::Access {
        permissions: vec!["storage".into()],
        optional: set(&["tabs", "alarms"], &["https://*.example.org/*"]),
        ..toolbar::Access::default()
    };
    let mut off = installed("off", None, toolbar::Access::default());
    off.enabled = false;
    controller.installed = vec![installed("permit", Some("popup.html"), access), off];
    // The engine learns what it may ask for.
    controller.tell_access(&shared);
    let grants = |window: &ui::webview::test_engine::TestEngine| {
        window
            .commands()
            .into_iter()
            .filter(|command| matches!(command, WebViewCommand::ExtensionGrants { .. }))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        grants(&window),
        [WebViewCommand::ExtensionGrants {
            id: "permit".into(),
            optional: set(&["tabs", "alarms"], &["https://*.example.org/*"]),
            granted: set(&[], &[]),
        }]
    );
    // Its request is the window's question, put as its addition was.
    let request = |request, id: &str, asked| WebViewEvent::ExtensionPermissionRequest {
        request,
        id: id.into(),
        asked,
    };
    assert!(controller.on_engine_event(
        request(3, "permit", set(&["tabs"], &["https://news.example.org/*"])),
        &shared
    ));
    assert_eq!(
        controller.take_questions(),
        [ExtensionQuestion {
            request: 3,
            name: "permit".into(),
            lines: vec![
                "Read and change your data on news.example.org".into(),
                "Read your browsing history".into(),
            ],
        }]
    );
    assert!(controller.take_questions().is_empty());
    // Nothing to ask about is granted, as in Chrome; an extension that is
    // off is refused.
    controller.on_engine_event(request(4, "permit", set(&["alarms"], &[])), &shared);
    controller.on_engine_event(request(5, "off", set(&["tabs"], &[])), &shared);
    assert!(controller.take_questions().is_empty());
    let decisions: Vec<_> = window
        .commands()
        .into_iter()
        .filter(|command| matches!(command, WebViewCommand::ExtensionPermissionDecision { .. }))
        .collect();
    assert_eq!(
        decisions,
        [
            WebViewCommand::ExtensionPermissionDecision {
                request: 4,
                allow: true
            },
            WebViewCommand::ExtensionPermissionDecision {
                request: 5,
                allow: false
            },
        ]
    );
    // What the engine granted it is kept, and not told back to it.
    let before = window.commands().len();
    controller.on_engine_event(
        WebViewEvent::ExtensionPermissions {
            id: "permit".into(),
            granted: set(&["tabs"], &[]),
        },
        &shared,
    );
    assert_eq!(controller.installed[0].access.granted, set(&["tabs"], &[]));
    controller.tell_access(&shared);
    assert_eq!(window.commands().len(), before);
}

#[test]
fn the_engine_learns_what_each_extension_may_do_as_it_changes() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let shortcuts = serde_json::json!({"_execute_action": {"suggested_key": "Alt+Shift+Y"}});
    let tabs = toolbar::Access {
        permissions: vec!["tabs".into()],
        hosts: vec!["https://*/*".into()],
        scriptable: vec!["<all_urls>".into()],
        framed: vec!["<all_urls>".into()],
        token: Some("a".repeat(48)),
        commands: Some(shortcuts.clone()),
        ..toolbar::Access::default()
    };
    controller.installed = vec![installed("reader", None, tabs)];
    controller.tell_access(&shared);
    let told = |window: &ui::webview::test_engine::TestEngine| {
        window
            .commands()
            .into_iter()
            .filter(|command| {
                matches!(
                    command,
                    WebViewCommand::ExtensionAccess { .. } | WebViewCommand::ExtensionForget { .. }
                )
            })
            .collect::<Vec<_>>()
    };
    // The Browser's own extension first: its worker reports what rules
    // acted on in the tabs, by a token of its own and with no access.
    let manager = extensions::manager_dir().expect("the manager is written");
    assert_eq!(
        told(&window),
        [
            WebViewCommand::ExtensionAccess {
                id: extensions::manager_id(&manager).expect("its id"),
                permissions: Vec::new(),
                hosts: Vec::new(),
                scriptable: Vec::new(),
                framed: Vec::new(),
                token: extensions::manager_token(&manager),
                commands: None,
            },
            WebViewCommand::ExtensionAccess {
                id: "reader".into(),
                permissions: vec!["tabs".into()],
                hosts: vec!["https://*/*".into()],
                scriptable: vec!["<all_urls>".into()],
                framed: vec!["<all_urls>".into()],
                token: Some("a".repeat(48)),
                commands: Some(shortcuts.clone()),
            }
        ]
    );
    controller.tell_access(&shared);
    assert_eq!(told(&window).len(), 2, "told once while it stays the same");
    // Turned off, it does not run: its keyboard shortcuts go back to the
    // page, and come back when it is turned on.
    let shortcuts_told = |window: &ui::webview::test_engine::TestEngine| match told(window).last() {
        Some(WebViewCommand::ExtensionAccess { id, commands, .. }) if id == "reader" => {
            commands.clone()
        }
        other => panic!("expected the reader's access, got {other:?}"),
    };
    controller.installed[0].enabled = false;
    controller.tell_access(&shared);
    assert_eq!(told(&window).len(), 3);
    assert_eq!(shortcuts_told(&window), None);
    controller.tell_access(&shared);
    assert_eq!(told(&window).len(), 3, "told once while it stays off");
    controller.installed[0].enabled = true;
    controller.tell_access(&shared);
    assert_eq!(told(&window).len(), 4);
    assert_eq!(shortcuts_told(&window), Some(shortcuts));
    controller.installed.clear();
    controller.tell_access(&shared);
    assert_eq!(
        told(&window).last(),
        Some(&WebViewCommand::ExtensionForget {
            id: "reader".into()
        })
    );
}

#[test]
fn the_person_sees_and_revokes_what_an_extension_was_allowed_later() {
    use ui::framework::HostDelivery as _;
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("permit");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(
        path.join("manifest.json"),
        r#"{"manifest_version":3,"name":"Permit","version":"1","permissions":["storage"],
        "optional_permissions":["tabs","alarms"],
        "optional_host_permissions":["https://*.example.org/*"]}"#,
    )
    .unwrap();
    let info = ExtensionInfo {
        id: "permit".into(),
        name: "Permit".into(),
        version: "1".into(),
        manifest_version: 3,
        description: String::new(),
        permissions: Vec::new(),
        path,
    };
    let permit = Installed::read(&info, true, false).unwrap();
    let id = permit.id.clone();
    let set = |permissions: &[&str], origins: &[&str]| ExtensionPermissionSet {
        permissions: permissions.iter().map(|&entry| entry.to_owned()).collect(),
        origins: origins.iter().map(|&entry| entry.to_owned()).collect(),
    };
    let mut controller = ExtensionController::new(true);
    controller.infos = vec![info];
    controller.installed = vec![permit];
    let row_text = |controller: &ExtensionController| {
        let rows = controller.library_rows(|_| true);
        let row = ui::widget::find_test_id(rows[0].as_ref(), &format!("browser.extension.{id}"))
            .expect("its row");
        row.accessibility()
            .and_then(|node| node.value)
            .unwrap_or_default()
    };
    let revocable = |controller: &ExtensionController| {
        let resource = controller.resources().remove(0);
        let rows = controller.library_rows(|_| true);
        (
            resource
                .actions
                .iter()
                .any(|spec| spec.id == "revoke-extension-access"),
            ui::widget::find_test_id(rows[0].as_ref(), &format!("browser.extension.revoke.{id}"))
                .is_some(),
        )
    };
    let run_queued = |controller: &mut ExtensionController| {
        let commands = std::mem::take(&mut *controller.commands.lock().unwrap());
        for command in commands {
            controller.run(command, &shared);
        }
    };
    // Allowed nothing later: nothing to list or revoke.
    assert!(!row_text(&controller).contains("Allowed later"));
    assert_eq!(revocable(&controller), (false, false));
    assert_eq!(
        controller.request(&ExtensionRequest::Revoke(id.clone())),
        Err(CATALOG.t("browser-extension-nothing-granted"))
    );

    // The person allowed it a site and its tabs later, and alarms, which
    // Chrome asks nobody about: its row and its resource say so, in the
    // words the question was put in.
    let allowed = set(&["tabs", "alarms"], &["https://news.example.org/*"]);
    controller.installed[0].access.granted = allowed.clone();
    controller.tell_access(&shared);
    let line =
        "Allowed later: Read and change your data on news.example.org, Read your browsing history";
    assert!(
        row_text(&controller).contains(line),
        "{}",
        row_text(&controller)
    );
    assert_eq!(
        controller.resources()[0]
            .presentation
            .as_ref()
            .and_then(|presentation| presentation.preview.as_deref()),
        Some(line)
    );
    assert_eq!(revocable(&controller), (true, true));

    // Its row's button revokes it: the engine holds the extension to its
    // manifest at once, and the Browser keeps that it holds none of it.
    let mut rows = controller.library_rows(|_| true);
    let pressed = rows[0]
        .deliver_control(
            ui::tilo_actions::OperationKind::Effect,
            &format!("browser.extension.revoke.{id}"),
            &mut automerge::AutoCommit::new(),
            &automerge::ROOT,
        )
        .unwrap();
    assert!(pressed.is_some(), "the row's button answers");
    assert_eq!(queued(&controller), [format!("Revoke({id:?})")]);
    let before = window.commands().len();
    run_queued(&mut controller);
    let optional = set(&["tabs", "alarms"], &["https://*.example.org/*"]);
    assert_eq!(
        window.commands()[before..],
        [WebViewCommand::ExtensionGrants {
            id: id.clone(),
            optional: optional.clone(),
            granted: set(&[], &[]),
        }]
    );
    assert!(controller.installed[0].access.granted.is_empty());
    // Told once: nothing is left to list, revoke or tell again.
    controller.tell_access(&shared);
    assert_eq!(window.commands().len(), before + 1);
    assert!(!row_text(&controller).contains("Allowed later"));
    assert_eq!(revocable(&controller), (false, false));

    // An agent or a screen reader revokes it through the resource alike.
    controller.installed[0].access.granted = allowed;
    controller.tell_access(&shared);
    let resource = controller.resources().remove(0);
    invoke(&mut controller, &resource, "revoke-extension-access").unwrap();
    let before = window.commands().len();
    run_queued(&mut controller);
    assert_eq!(
        window.commands()[before..],
        [WebViewCommand::ExtensionGrants {
            id,
            optional,
            granted: set(&[], &[]),
        }]
    );
}
