use super::*;

mod access;
mod omnibox;
mod side_panel;
mod sign_in;

const STORE_ITEM: &str =
    "https://chromewebstore.google.com/detail/ublock-origin-lite/ddkjiahejlhfcafbddmgiahcphecmpfh";

fn queued(controller: &ExtensionController) -> Vec<String> {
    controller
        .commands
        .lock()
        .unwrap()
        .iter()
        .map(|command| format!("{command:?}"))
        .collect()
}

#[test]
fn the_window_bar_offers_a_store_item_and_opens_extension_popups() {
    let mut controller = ExtensionController::new(true);
    assert_eq!(controller.bar_offer(), None, "no offer away from the store");
    controller.store_item = web_store::item_from_url(STORE_ITEM);
    let offer = controller.bar_offer().expect("a store item is offered");
    assert_eq!(offer.id, BAR_ADD);
    assert!(offer.enabled);
    assert!(controller.bar_action(BAR_ADD));
    assert_eq!(
        queued(&controller),
        [r#"Fetch("ddkjiahejlhfcafbddmgiahcphecmpfh")"#]
    );
    controller.busy = true;
    assert!(
        !controller.bar_offer().unwrap().enabled,
        "one download at a time"
    );

    assert!(controller.bar_action(&format!("{BAR_OPEN}abc")));
    assert_eq!(queued(&controller)[1], r#"OpenAction("abc")"#);
    assert!(!controller.bar_action("reload"), "not this controller's");

    let private = ExtensionController::new(false);
    assert_eq!(private.bar_offer(), None);
    assert!(private.bar_items().is_empty());
}

#[test]
fn a_popup_is_as_big_as_its_page_and_closes_when_its_page_closes_it() {
    let window: SharedEngine = ui::webview::test_engine::TestEngine::new();
    let page = ui::webview::test_engine::TestEngine::new();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    controller.panel = Some(Panel::Popup {
        id: "abc".into(),
        engine: page.clone(),
        size: None,
        site: None,
        beneath: None,
    });
    let shown = |controller: &ExtensionController| match &controller.panel {
        Some(Panel::Popup { size, .. }) => *size,
        _ => None,
    };
    page.push_event(ui::webview::WebViewEvent::DocumentSize {
        width: 380.0,
        height: 430.0,
        scroll_y: 0.0,
    });
    assert!(controller.sync(&window, ""));
    assert_eq!(shown(&controller), Some((380.0, 430.0)));
    // Never past Chrome's largest popup.
    page.push_event(ui::webview::WebViewEvent::DocumentSize {
        width: 2000.0,
        height: 2000.0,
        scroll_y: 0.0,
    });
    controller.sync(&window, "");
    assert_eq!(shown(&controller), Some(POPUP_MAX));
    // The panel is as wide as its page, within the window's room.
    assert_eq!(controller.page_width(1200.0), Some(POPUP_MAX.0));
    assert_eq!(controller.page_width(500.0), Some(500.0));
    // Its page called window.close(): the panel goes.
    page.push_event(ui::webview::WebViewEvent::TabClosed);
    assert!(controller.sync(&window, ""));
    assert!(controller.panel.is_none());
}

fn installed(id: &str, popup: Option<&str>, access: toolbar::Access) -> Installed {
    Installed {
        id: id.into(),
        name: id.into(),
        root: std::path::PathBuf::new(),
        enabled: true,
        pinned: true,
        action: Some(toolbar::ActionManifest {
            popup: popup.map(str::to_owned),
            ..toolbar::ActionManifest::default()
        }),
        options: None,
        access,
        keyword: None,
    }
}

#[test]
fn an_extensions_button_menu_shows_its_items_and_a_choice_reaches_it() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let id = "a".repeat(32);
    controller.installed = vec![installed(&id, None, toolbar::Access::default())];
    let command =
        |key: &str, label: &str, checked: Option<bool>, enabled| ExtensionMenuItem::Command {
            key: key.into(),
            label: label.into(),
            checked,
            enabled,
        };
    let items = vec![
        command("\"dark\"", "Dark pages", Some(true), true),
        ExtensionMenuItem::Separator,
        ExtensionMenuItem::Submenu {
            label: "Lists".into(),
            enabled: true,
            items: vec![
                command("1", "Easy", Some(false), true),
                command("2", "Strict", Some(true), false),
            ],
        },
    ];
    let told = WebViewEvent::ExtensionMenu {
        id: id.clone(),
        items,
    };
    assert!(controller.on_engine_event(told.clone(), &shared));
    assert!(!controller.on_engine_event(told, &shared), "nothing new");

    // The Extensions menu: its own rows, then the extension's.
    let mut menu = controller.extensions_menu(&controller.model());
    let rows = menu.menu_mut().items().to_vec();
    assert_eq!(rows.len(), 3, "{rows:?}");
    assert_eq!(rows[2].label(), Some(format!("More from {id}").as_str()));
    let nested = rows[2].submenu_items().expect("its items open beside it");
    assert_eq!(
        nested[0].checked(),
        Some(true),
        "a check mark on a checked item"
    );
    assert!(matches!(nested[1], MenuItem::Separator));
    let lists = nested[2].submenu_items().expect("a submenu of its own");
    assert_eq!(lists[0].checked(), Some(false));
    assert!(!lists[1].is_enabled(), "a disabled item is greyed");
    let MenuItem::Action { action, .. } = &lists[0] else {
        panic!("a command row");
    };
    action();
    assert_eq!(
        queued(&controller).last().map(String::as_str),
        Some(format!(r#"ChooseMenuItem("{id}", "1")"#).as_str())
    );
    let commands = std::mem::take(&mut *controller.commands.lock().unwrap());
    for command in commands {
        controller.run(command, &shared);
    }
    assert!(
        window
            .commands()
            .contains(&WebViewCommand::ExtensionMenuChoice {
                id: id.clone(),
                key: "1".into()
            })
    );

    // The window bar's menu is one level deep: the submenu's items stand
    // in its place, after the extension's own item.
    let bar = controller.bar_items();
    let names: Vec<_> = bar.iter().map(|item| item.name.as_str()).collect();
    assert_eq!(names, [id.as_str(), "Dark pages", "Easy", "Strict"]);
    assert!(bar[1].on && bar[1].toggles);
    assert!(!bar[3].enabled);
    assert!(controller.bar_action(&bar[2].id));
    assert_eq!(
        queued(&controller).last().map(String::as_str),
        Some(format!(r#"ChooseMenuItem("{id}", "1")"#).as_str())
    );

    // Taken away: the menu row goes.
    assert!(controller.on_engine_event(
        WebViewEvent::ExtensionMenu {
            id,
            items: Vec::new()
        },
        &shared
    ));
    let mut menu = controller.extensions_menu(&controller.model());
    assert_eq!(menu.menu_mut().items().len(), 2);
    assert_eq!(controller.bar_items().len(), 1);
}

#[test]
fn a_button_without_a_popup_tells_the_extension_of_the_click() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.installed = vec![installed("clicker", None, toolbar::Access::default())];
    controller.open_action("clicker", &shared);
    assert!(
        controller.panel.is_none(),
        "nothing to show: the page is acted on"
    );
    assert!(window.commands().contains(&WebViewCommand::ExtensionClick {
        id: "clicker".into()
    }));
    assert!(window.created_tabs().is_empty());
}

#[test]
fn a_buttons_keyboard_shortcut_acts_as_a_click_on_it() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let (clicker, opener) = ("c".repeat(32), "d".repeat(32));
    controller.installed = vec![
        installed(&clicker, None, toolbar::Access::default()),
        installed(&opener, Some("popup.html"), toolbar::Access::default()),
    ];
    let pressed = |id: &str| WebViewEvent::ExtensionExecuteAction { id: id.into() };

    // Without a popup, the extension hears of a click.
    assert!(controller.on_engine_event(pressed(&clicker), &shared));
    assert!(window.commands().contains(&WebViewCommand::ExtensionClick {
        id: clicker.clone()
    }));

    // With one, its popup opens over the page, which it may script, as
    // after a click; pressed again, it closes.
    assert!(controller.on_engine_event(pressed(&opener), &shared));
    let created = window.created_tabs();
    assert_eq!(created.len(), 1);
    let commands = created[0].commands();
    assert!(commands.contains(&WebViewCommand::ExtensionPanel {
        action: Some(opener.clone())
    }));
    assert!(commands.contains(&WebViewCommand::Navigate(format!(
        "chrome-extension://{opener}/popup.html"
    ))));
    controller.on_engine_event(pressed(&opener), &shared);
    assert!(controller.panel.is_none());

    // One turned off has no button to act as.
    controller.installed[0].enabled = false;
    let clicks = || {
        window
            .commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::ExtensionClick { .. }))
            .count()
    };
    let before = clicks();
    controller.on_engine_event(pressed(&clicker), &shared);
    assert_eq!(clicks(), before);
}

#[test]
fn the_toolbar_shows_what_an_extension_set_for_this_page() {
    use ui::webview::ExtensionActionChange as Change;
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    let id = "a".repeat(32);
    let popup = format!("chrome-extension://{id}/popup.html");
    controller.installed = vec![installed(
        &id,
        Some("popup.html"),
        toolbar::Access::default(),
    )];
    let event = |global, change| WebViewEvent::ExtensionAction {
        id: id.clone(),
        global,
        change,
    };
    let button = |controller: &ExtensionController| controller.model().pinned.remove(0);
    let clicks = |window: &ui::webview::test_engine::TestEngine| {
        window
            .commands()
            .iter()
            .filter(|command| matches!(command, WebViewCommand::ExtensionClick { .. }))
            .count()
    };

    // An ad blocker's count for this page, and no popup on it.
    let page = Change {
        badge: Some(Some("12".into())),
        popup: Some(Some(String::new())),
        ..Change::default()
    };
    assert!(controller.on_engine_event(event(false, page.clone()), &shared));
    assert!(
        !controller.on_engine_event(event(false, page), &shared),
        "nothing new"
    );
    let shown = button(&controller);
    assert_eq!((shown.badge.as_str(), shown.popup), ("12", None));
    controller.open_action(&id, &shared);
    assert_eq!(clicks(&window), 1, "a click goes to the extension");
    assert!(window.created_tabs().is_empty());

    // Turned off on this page: a click does nothing.
    controller.on_engine_event(
        event(
            false,
            Change {
                enabled: Some(Some(false)),
                ..Change::default()
            },
        ),
        &shared,
    );
    assert!(!button(&controller).enabled);
    controller.open_action(&id, &shared);
    assert_eq!(clicks(&window), 1);

    // Its icon for every tab outlasts the page; the page's values do not.
    controller.on_engine_event(
        event(
            true,
            Change {
                icon: Some(Some(ExtensionActionIcon::Pixels {
                    width: 1,
                    height: 1,
                    rgba: vec![1, 2, 3, 4],
                })),
                ..Change::default()
            },
        ),
        &shared,
    );
    assert_eq!(
        controller.action_icon(&id).map(|image| image.width),
        Some(1)
    );
    assert!(controller.on_engine_event(WebViewEvent::ExtensionActionReset, &shared));
    let shown = button(&controller);
    assert_eq!(
        (shown.badge.as_str(), shown.popup.as_deref(), shown.enabled),
        ("", Some(popup.as_str()), true)
    );
    assert!(controller.action_icon(&id).is_some());

    // It asks to open its popup: shown over the page, without the access
    // a click gives.
    assert!(controller.on_engine_event(
        WebViewEvent::ExtensionOpenPopup {
            id: id.clone(),
            url: popup.clone(),
        },
        &shared,
    ));
    let created = window.created_tabs();
    assert_eq!(created.len(), 1);
    let commands = created[0].commands();
    assert!(commands.contains(&WebViewCommand::ExtensionPanel { action: None }));
    assert!(commands.contains(&WebViewCommand::Navigate(popup.clone())));

    // One turned off asks for nothing.
    controller.panel = None;
    controller.installed[0].enabled = false;
    assert!(!controller.on_engine_event(
        WebViewEvent::ExtensionOpenPopup {
            id: id.clone(),
            url: popup
        },
        &shared,
    ));
    assert_eq!(window.created_tabs().len(), 1);
}

#[test]
fn an_icon_told_before_the_profile_is_read_shows_once_it_is() {
    // A window opened beside running extensions is told the icons they
    // set for every tab as its page opens, before its read of the profile
    // says where their files are.
    let window: SharedEngine = ui::webview::test_engine::TestEngine::new();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    let id = "a".repeat(32);
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("off.png"),
        ui::png::encode_rgba8(2, 1, &[1, 2, 3, 255, 5, 6, 7, 255]).unwrap(),
    )
    .unwrap();
    assert!(controller.on_engine_event(
        WebViewEvent::ExtensionAction {
            id: id.clone(),
            global: true,
            change: ui::webview::ExtensionActionChange {
                icon: Some(Some(ExtensionActionIcon::Path("/off.png".into()))),
                ..ui::webview::ExtensionActionChange::default()
            },
        },
        &window,
    ));
    assert!(controller.action_icon(&id).is_none(), "nothing to read yet");

    controller.apply_profile(Profile {
        infos: Vec::new(),
        installed: vec![Installed {
            root: root.path().to_owned(),
            ..installed(&id, None, toolbar::Access::default())
        }],
        wanted: Vec::new(),
        icons: Vec::new(),
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while controller.action_icon(&id).is_none() && std::time::Instant::now() < deadline {
        controller.sync(&window, "");
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        controller
            .action_icon(&id)
            .map(|image| (image.width, image.height)),
        Some((2, 1)),
        "the icon it set, not its manifest's"
    );
}

#[test]
fn a_popup_is_no_tab_and_finds_the_page_it_opened_over() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.installed = vec![installed(
        "asker",
        Some("popup.html"),
        toolbar::Access::default(),
    )];
    controller.open_action("asker", &shared);
    let created = window.created_tabs();
    assert_eq!(created.len(), 1, "the popup's page");
    let page = &created[0];
    let commands = page.commands();
    let panel = commands
        .iter()
        .position(|c| matches!(c, WebViewCommand::ExtensionPanel { .. }));
    let navigate = commands.iter().position(|c| matches!(c, WebViewCommand::Navigate(url) if url == "chrome-extension://asker/popup.html"));
    assert!(
        panel.is_some() && panel < navigate,
        "marked before its page loads: {commands:?}"
    );
    assert_eq!(
        commands[panel.unwrap()],
        WebViewCommand::ExtensionPanel {
            action: Some("asker".into())
        },
        "opened by its button, it may script the page"
    );
}

/// Invoke the action `action` of `resource` as a caller of the door
/// does: its bound payload, decoded as the window decodes it.
fn invoke(
    controller: &mut ExtensionController,
    resource: &Resource,
    action: &str,
) -> Result<(), String> {
    invoke_operation(resource, action, |operation| match operation {
        crate::effects::BrowserOperation::Extension(request) => controller.request(&request),
        other => Err(format!("{other:?} is no extension request")),
    })
}

/// The operation the action `action` of `resource` decodes to, as a
/// caller of the door sends it, handed to `run`.
fn invoke_operation(
    resource: &Resource,
    action: &str,
    run: impl FnOnce(crate::effects::BrowserOperation) -> Result<(), String>,
) -> Result<(), String> {
    use ui::tilo_resource::Effect as _;
    let spec = resource
        .actions
        .iter()
        .find(|spec| spec.id == action)
        .ok_or_else(|| format!("{action} is not offered"))?;
    let encoded = spec
        .build_json(&serde_json::Map::new())
        .map_err(|error| format!("{error:?}"))?;
    let effect = crate::effects::BrowserEffect::from_payload(&encoded.payload)?;
    run(effect.operation())
}

#[test]
fn a_script_or_an_agent_manages_extensions_as_the_library_does() {
    let profile = tempfile::tempdir().unwrap();
    let path = profile.path().join("lite");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(
        path.join("manifest.json"),
        r#"{"manifest_version":3,"name":"Lite","version":"1",
        "action":{"default_popup":"popup.html"},"options_page":"options.html"}"#,
    )
    .unwrap();
    let info = ExtensionInfo {
        id: "lite".into(),
        name: "Lite".into(),
        version: "1".into(),
        manifest_version: 3,
        description: String::new(),
        permissions: Vec::new(),
        path,
    };
    let on = Installed::read(&info, true, false).unwrap();
    let id = on.id.clone();
    let mut controller = ExtensionController::new(true);
    controller.infos = vec![info];
    controller.installed = vec![on.clone()];
    let actions = |controller: &ExtensionController| {
        let resources = controller.resources();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].id.0, "browser.extension:lite");
        resources[0]
            .actions
            .iter()
            .map(|spec| spec.id.clone())
            .collect::<Vec<_>>()
    };
    // On: what its Library row offers.
    assert_eq!(
        actions(&controller),
        [
            "show-extensions",
            "turn-off-extension",
            "use-extension",
            "open-extension-settings",
            "pin-extension",
            "remove-extension"
        ]
    );
    let resource = controller.resources().remove(0);
    for action in [
        "turn-off-extension",
        "use-extension",
        "pin-extension",
        "open-extension-settings",
        "remove-extension",
    ] {
        invoke(&mut controller, &resource, action).unwrap();
    }
    assert_eq!(
        queued(&controller),
        [
            format!("Disable({id:?})"),
            format!("OpenAction({id:?})"),
            format!("Pin({id:?}, true)"),
            format!("OpenSettings({id:?})"),
            format!("Remove({id:?})"),
        ]
    );

    // Off: it can be turned on, and has no settings or button meanwhile.
    controller.installed = vec![Installed {
        enabled: false,
        ..on
    }];
    assert_eq!(
        actions(&controller),
        ["show-extensions", "turn-on-extension", "remove-extension"]
    );
    let off = controller.resources().remove(0);
    invoke(&mut controller, &off, "turn-on-extension").unwrap();
    assert_eq!(queued(&controller).last(), Some(&format!("Enable({id:?})")));
    assert!(
        invoke(&mut controller, &resource, "open-extension-settings").is_err(),
        "no settings page while it is off"
    );
    assert!(
        invoke(&mut controller, &resource, "use-extension").is_err(),
        "no button while it is off"
    );
    assert!(
        controller
            .request(&ExtensionRequest::Remove("unknown".into()))
            .is_err()
    );
    assert!(
        ExtensionController::new(false)
            .request(&ExtensionRequest::TurnOn(id))
            .is_err(),
        "a private window has no extensions"
    );
}

/// Adding from the Web Store is the extensions' action on a store item
/// not added yet; it shows the offer the person adds it from.
#[test]
fn adding_from_the_store_is_an_action_on_a_store_item_only() {
    let available = |controller: &ExtensionController| {
        controller.add_action().map(|spec| {
            matches!(
                spec.contract.availability,
                ui::tilo_actions::Availability::Available
            )
        })
    };
    let mut controller = ExtensionController::new(true);
    assert_eq!(available(&controller), Some(false), "away from the store");
    assert!(controller.offer_store_item().is_err());
    controller.store_item = web_store::item_from_url(STORE_ITEM);
    assert_eq!(available(&controller), Some(true));
    let extensions =
        ui::tilo_actions::Resource::new("browser.extensions", "browser.extensions", "Extensions")
            .with_action(controller.add_action().unwrap());
    invoke_operation(&extensions, "add-extension", |operation| match operation {
        crate::effects::BrowserOperation::AddExtension => controller.offer_store_item(),
        other => Err(format!("{other:?} is not adding")),
    })
    .unwrap();
    assert_eq!(
        queued(&controller),
        [r#"Fetch("ddkjiahejlhfcafbddmgiahcphecmpfh")"#],
        "fetched to show what it may do, as the button does"
    );
    controller.busy = true;
    assert_eq!(available(&controller), Some(false));
    assert!(controller.offer_store_item().is_err(), "one at a time");
    controller.busy = false;
    let id = controller.store_item.as_ref().unwrap().id.clone();
    controller.installed = vec![installed(&id, None, toolbar::Access::default())];
    assert_eq!(
        controller
            .add_action()
            .map(|spec| spec.contract.availability),
        Some(ui::tilo_actions::Availability::Unavailable {
            code: "added".into(),
            reason: "This extension is already added".into(),
        }),
        "on the store page of one already added, it says so"
    );
    assert_eq!(
        controller.offer_store_item(),
        Err("This extension is already added".into())
    );
    assert_eq!(
        ExtensionController::new(false).add_action().map(|_| ()),
        None
    );
}

/// A failed add or change says what happened and what to do; the raw
/// file, archive or network error is for the log only.
#[test]
fn a_failure_says_what_happened_and_what_to_do() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        read_package_file(&folder.path().join("absent.crx")),
        Err("No file was found at this path. Check the path and try again.".into())
    );
    assert_eq!(
        read_package_file(folder.path()),
        Err("This file could not be opened. Check that you may read it and try again.".into()),
        "a folder is no package file"
    );

    let not_a_zip = extensions::stage_file(b"not a zip".to_vec()).err().unwrap();
    assert_eq!(
        add_failed(&not_a_zip),
        "This extension package is damaged or incomplete. Download a fresh copy and try again."
    );
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        use std::io::Write as _;
        let mut zip = zip::ZipWriter::new(&mut buf);
        zip.start_file("readme.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"hi").unwrap();
        zip.finish().unwrap();
    }
    let no_manifest = extensions::stage_file(buf.into_inner()).err().unwrap();
    assert_eq!(
        add_failed(&no_manifest),
        "This extension is broken and cannot be added. Look for a newer version from its maker."
    );
    assert_eq!(
        add_failed(&ExtensionError::Download("error sending request".into())),
        "The extension could not be downloaded from the Chrome Web Store. \
         Check your connection and try again."
    );
    assert_eq!(
        add_failed(&ExtensionError::NoPackage),
        CATALOG.t("browser-extension-no-version")
    );

    let refused = refusal(
        &ExtensionError::Io(std::io::ErrorKind::PermissionDenied.into()),
        "browser-extension-pin-failed",
        &[
            ("name", ui::i18n::ArgValue::Text("Lite".into())),
            ("pinned", ui::i18n::ArgValue::Text(false.to_string())),
        ],
    );
    assert_eq!(
        refused,
        "Lite could not be unpinned from the toolbar. Try again."
    );
}

#[test]
fn a_removed_extensions_page_and_update_notice_go_with_it() {
    let mut controller = ExtensionController::new(true);
    controller.panel = Some(Panel::Popup {
        id: "settings:abc".into(),
        engine: ui::webview::test_engine::TestEngine::new(),
        size: None,
        site: None,
        beneath: None,
    });
    controller.awaiting = vec!["abc".into()];
    let profile = |installed: Vec<Installed>| Profile {
        infos: Vec::new(),
        installed,
        wanted: Vec::new(),
        icons: Vec::new(),
    };
    let off = Installed {
        enabled: false,
        ..installed("abc", None, toolbar::Access::default())
    };
    // Still there, but off: its page closes, its update still waits.
    controller.apply_profile(profile(vec![off]));
    assert!(controller.panel.is_none());
    assert_eq!(controller.awaiting, ["abc"]);
    // Removed: nothing is left waiting for it.
    controller.apply_profile(profile(Vec::new()));
    assert!(controller.awaiting.is_empty());
}

#[test]
fn each_library_row_acts_on_its_own_extension() {
    let profile = tempfile::tempdir().unwrap();
    let record = |name: &str, manifest: &str| {
        let path = profile.path().join(name);
        std::fs::create_dir(&path).unwrap();
        std::fs::write(path.join("manifest.json"), manifest).unwrap();
        ExtensionInfo {
            id: name.into(),
            name: name.into(),
            version: "1".into(),
            manifest_version: 3,
            description: String::new(),
            permissions: Vec::new(),
            path,
        }
    };
    let infos = vec![
        record(
            "broken",
            r#"{"manifest_version":3,"name":"broken","version":"1","key":"not base64!"}"#,
        ),
        record(
            "kept",
            r#"{"manifest_version":3,"name":"kept","version":"1"}"#,
        ),
    ];
    let (infos, installed) = paired(infos);
    assert_eq!(infos.len(), installed.len());
    assert_eq!(infos[0].name, "kept");
    assert_eq!(
        Some(installed[0].id.clone()),
        extensions::chrome_id(&infos[0]),
        "the row's buttons are the kept extension's"
    );
}

#[test]
fn an_updated_extension_is_read_again_so_its_new_version_runs() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("manifest.json"),
        r#"{"manifest_version":3,"name":"Lite","version":"2"}"#,
    )
    .unwrap();
    let info = ExtensionInfo {
        id: "lite".into(),
        name: "Lite".into(),
        version: "2".into(),
        manifest_version: 3,
        description: String::new(),
        permissions: Vec::new(),
        path: root.path().to_owned(),
    };
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    controller.on_updated(extensions::Updated::Installed(info.clone()));
    assert!(controller.refresh, "the profile is read again");
    assert!(controller.awaiting.is_empty());
    controller.refresh = false;
    controller.on_updated(extensions::Updated::AwaitsApproval(info));
    assert!(controller.refresh);
    assert_eq!(controller.awaiting, ["Lite"]);
}

#[test]
fn a_tilo_windows_bar_shows_every_extensions_badge() {
    let extension = |id: &str, pinned: bool, action: bool| Installed {
        id: id.into(),
        name: id.into(),
        root: std::path::PathBuf::new(),
        enabled: true,
        pinned,
        action: action.then(crate::extension_toolbar::ActionManifest::default),
        options: None,
        access: crate::extension_toolbar::Access::default(),
        keyword: None,
    };
    let installed = [
        extension("pinned", true, true),
        extension("listed", false, true),
        extension("actionless", false, false),
    ];
    assert_eq!(badges_shown(&installed, true), ["pinned", "listed"]);
    assert_eq!(badges_shown(&installed, false), ["pinned"]);
}

#[test]
fn turning_an_extension_off_keeps_it_installed_and_its_data() {
    let extension = |id: &str, enabled: bool| Installed {
        id: id.into(),
        name: id.into(),
        root: std::path::PathBuf::new(),
        enabled,
        pinned: false,
        action: None,
        options: None,
        access: crate::extension_toolbar::Access::default(),
        keyword: None,
    };
    let mut switched = HashMap::new();
    // At start each is put in the state the person chose, once.
    let installed = [extension("blocker", true), extension("sleeper", false)];
    assert_eq!(
        switches_for(&installed, &mut switched),
        [("blocker".to_owned(), true), ("sleeper".to_owned(), false)]
    );
    assert!(switches_for(&installed, &mut switched).is_empty());
    // Turned off: switched off, never uninstalled.
    let installed = [extension("blocker", false), extension("sleeper", false)];
    assert_eq!(
        switches_for(&installed, &mut switched),
        [("blocker".to_owned(), false)]
    );
    assert_eq!(
        manager_url(
            "mgr",
            &[("a".into(), true), ("b".into(), false), ("c".into(), false)]
        ),
        "chrome-extension://mgr/manage.html?on=a&off=b,c"
    );
    // The host is asked to uninstall nothing for it.
    let mut controller = ExtensionController::new(true);
    controller.host.reconcile(&[], &[], ui::time::now());
    assert!(
        controller
            .host
            .take_outbox()
            .iter()
            .all(|request| request.method != "Extensions.uninstall")
    );
}
