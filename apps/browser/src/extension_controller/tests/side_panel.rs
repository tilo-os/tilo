//! An extension's side panel beside the window's page: what the extension
//! asks of it, and the person opening and closing it from the window and
//! through the extension's actions.

use super::*;
use ui::webview::ExtensionSidePanelChange;

fn with_side_panel(id: &str) -> Installed {
    installed(
        id,
        None,
        toolbar::Access {
            permissions: vec!["sidePanel".into()],
            ..toolbar::Access::default()
        },
    )
}

#[test]
fn an_extensions_side_panel_shows_beside_the_page_as_it_asks() {
    use ExtensionSidePanelChange as Change;
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    let (notes, other) = ("n".repeat(32), "o".repeat(32));
    controller.installed = vec![with_side_panel(&notes), with_side_panel(&other)];
    let page = |id: &str, path: &str| format!("chrome-extension://{id}/{path}");
    let asked = |controller: &mut ExtensionController, id: &str, change| {
        controller.on_engine_event(
            WebViewEvent::ExtensionSidePanel {
                id: id.into(),
                change,
            },
            &shared,
        )
    };

    // Opened: its page loads in a tab of its own, which is no tab to
    // extensions and finds the page beside it as its window; the host tells
    // the extension of it opening and closing.
    assert!(asked(
        &mut controller,
        &notes,
        Change::Open(page(&notes, "panel.html"))
    ));
    assert_eq!(controller.side_panel_shown(), Some(notes.as_str()));
    let created = window.created_tabs();
    assert_eq!(created.len(), 1);
    assert_eq!(
        created[0].commands(),
        [
            WebViewCommand::ExtensionSidePanelPage { id: notes.clone() },
            WebViewCommand::Navigate(page(&notes, "panel.html")),
        ]
    );
    assert!(
        controller
            .side_panel(side_panel_width(&ui::theme::Theme::dark()))
            .is_some()
    );

    // Its options name another page for this tab: the panel shows it,
    // in the same tab; turned off for the tab, it closes.
    assert!(!asked(
        &mut controller,
        &notes,
        Change::Options(Some(page(&notes, "other.html")))
    ));
    assert_eq!(window.created_tabs().len(), 1);
    assert_eq!(
        created[0].commands().last(),
        Some(&WebViewCommand::Navigate(page(&notes, "other.html")))
    );
    assert!(asked(&mut controller, &notes, Change::Options(None)));
    assert_eq!(controller.side_panel_shown(), None);
    assert!(
        controller
            .side_panel(side_panel_width(&ui::theme::Theme::dark()))
            .is_none()
    );
    // Options and closing where it does not show change nothing.
    assert!(!asked(
        &mut controller,
        &notes,
        Change::Options(Some(page(&notes, "p.html")))
    ));
    assert!(!asked(&mut controller, &notes, Change::Close));
    assert_eq!(window.created_tabs().len(), 1);

    // A click on its button opens it, and the next one closes it.
    assert!(asked(
        &mut controller,
        &notes,
        Change::Toggle(page(&notes, "panel.html"))
    ));
    assert_eq!(controller.side_panel_shown(), Some(notes.as_str()));
    assert!(asked(
        &mut controller,
        &notes,
        Change::Toggle(page(&notes, "panel.html"))
    ));
    assert_eq!(controller.side_panel_shown(), None);

    // One side panel at a time: another extension's takes its place.
    asked(
        &mut controller,
        &notes,
        Change::Open(page(&notes, "panel.html")),
    );
    assert!(asked(
        &mut controller,
        &other,
        Change::Open(page(&other, "panel.html"))
    ));
    assert_eq!(controller.side_panel_shown(), Some(other.as_str()));
    assert!(
        !asked(&mut controller, &notes, Change::Close),
        "not the one that shows"
    );
    assert!(asked(&mut controller, &other, Change::Close));

    // Its page closed itself (window.close()): the panel goes.
    asked(
        &mut controller,
        &notes,
        Change::Open(page(&notes, "panel.html")),
    );
    let shown = window.created_tabs().last().unwrap().clone();
    shown.push_event(WebViewEvent::TabClosed);
    assert!(controller.sync(&shared, ""));
    assert_eq!(controller.side_panel_shown(), None);

    // An extension turned off shows none.
    controller.installed[0].enabled = false;
    let before = window.created_tabs().len();
    assert!(!asked(
        &mut controller,
        &notes,
        Change::Open(page(&notes, "panel.html"))
    ));
    assert_eq!(window.created_tabs().len(), before);
}

#[test]
fn the_person_opens_and_closes_a_side_panel_from_the_window_and_the_door() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    let notes = "n".repeat(32);
    controller.installed = vec![
        with_side_panel(&notes),
        installed(
            &"p".repeat(32),
            Some("popup.html"),
            toolbar::Access::default(),
        ),
    ];
    // Only an extension that may show one offers its side panel.
    let toggles = |controller: &ExtensionController| {
        controller
            .bar_items()
            .into_iter()
            .filter(|item| item.id.starts_with(BAR_SIDE_PANEL))
            .map(|item| (item.id, item.on))
            .collect::<Vec<_>>()
    };
    let toggle = format!("{BAR_SIDE_PANEL}{notes}");
    assert_eq!(toggles(&controller), [(toggle.clone(), false)]);
    assert!(controller.bar_side_panel().is_none());

    // Asked from the window's bar, the engine asks the extension which
    // page its panel shows beside this page.
    assert!(controller.bar_action(&toggle));
    controller.sync(&shared, "");
    assert!(
        window
            .commands()
            .contains(&WebViewCommand::ExtensionOpenSidePanel { id: notes.clone() })
    );
    controller.on_engine_event(
        WebViewEvent::ExtensionSidePanel {
            id: notes.clone(),
            change: ExtensionSidePanelChange::Open(format!(
                "chrome-extension://{notes}/panel.html"
            )),
        },
        &shared,
    );
    assert_eq!(toggles(&controller), [(toggle.clone(), true)]);
    let close = controller
        .bar_side_panel()
        .expect("the bar closes the side panel");
    assert!(close.on);

    // The bar's button closes it.
    assert!(controller.bar_action(&close.id));
    controller.sync(&shared, "");
    assert_eq!(controller.side_panel_shown(), None);

    // A script or an agent does the same through the extension's actions.
    let actions = |controller: &ExtensionController| {
        controller
            .resources()
            .into_iter()
            .flat_map(|resource| resource.actions)
            .map(|spec| spec.id.clone())
            .filter(|id| id.contains("side-panel"))
            .collect::<Vec<_>>()
    };
    controller.infos = controller
        .installed
        .iter()
        .map(|extension| ExtensionInfo {
            id: extension.id.clone(),
            name: extension.name.clone(),
            version: "1".into(),
            manifest_version: 3,
            description: String::new(),
            permissions: extension.access.permissions.clone(),
            path: std::path::PathBuf::new(),
        })
        .collect();
    assert_eq!(actions(&controller), ["open-extension-side-panel"]);
    assert!(
        controller
            .request(&ExtensionRequest::OpenSidePanel(notes.clone()))
            .is_ok()
    );
    assert!(
        controller
            .request(&ExtensionRequest::OpenSidePanel("p".repeat(32)))
            .is_err(),
        "no side panel to open"
    );
    controller.on_engine_event(
        WebViewEvent::ExtensionSidePanel {
            id: notes.clone(),
            change: ExtensionSidePanelChange::Open(format!(
                "chrome-extension://{notes}/panel.html"
            )),
        },
        &shared,
    );
    assert_eq!(actions(&controller), ["close-extension-side-panel"]);
    controller
        .request(&ExtensionRequest::CloseSidePanel(notes))
        .unwrap();
    controller.sync(&shared, "");
    assert_eq!(controller.side_panel_shown(), None);
}

#[test]
fn a_side_panel_asks_for_more_access_through_the_window() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    let notes = "n".repeat(32);
    let set = |permissions: &[&str]| ExtensionPermissionSet {
        permissions: permissions.iter().map(|&entry| entry.to_owned()).collect(),
        origins: Vec::new(),
    };
    controller.installed = vec![installed(
        &notes,
        None,
        toolbar::Access {
            permissions: vec!["sidePanel".into()],
            optional: set(&["tabs"]),
            ..toolbar::Access::default()
        },
    )];
    controller.on_engine_event(
        WebViewEvent::ExtensionSidePanel {
            id: notes.clone(),
            change: ExtensionSidePanelChange::Open(format!(
                "chrome-extension://{notes}/panel.html"
            )),
        },
        &shared,
    );
    let panel = window.created_tabs().last().expect("its page").clone();

    // Its page calls permissions.request: the host asks on the page that
    // asks, which is the panel's, and the person must see the question, or
    // the extension waits for good and may ask nothing again.
    panel.push_event(WebViewEvent::ExtensionPermissionRequest {
        request: 7,
        id: notes.clone(),
        asked: set(&["tabs"]),
    });
    assert!(controller.sync(&shared, ""));
    assert_eq!(
        controller
            .take_questions()
            .into_iter()
            .map(|question| question.request)
            .collect::<Vec<_>>(),
        [7]
    );
    // What the engine then granted it, told on the panel's page, is kept.
    panel.push_event(WebViewEvent::ExtensionPermissions {
        id: notes.clone(),
        granted: set(&["tabs"]),
    });
    controller.sync(&shared, "");
    assert_eq!(controller.installed[0].access.granted, set(&["tabs"]));
    assert_eq!(controller.side_panel_shown(), Some(notes.as_str()));
}

#[test]
fn a_side_panel_is_as_wide_as_the_person_dragged_it_within_the_themes_bounds() {
    let theme = ui::theme::Theme::dark();
    let (narrowest, widest) = side_panel_bounds(&theme);
    assert!(narrowest < side_panel_width(&theme) && side_panel_width(&theme) < widest);
    let shared: SharedEngine = ui::webview::test_engine::TestEngine::new();
    let dir = tempfile::tempdir().unwrap();
    let store = || {
        ui::storage::Settings::<SidePanelWidths>::for_app("so.tilo.browser")
            .in_file(dir.path().join("settings.json"))
    };
    let (notes, other) = ("n".repeat(32), "o".repeat(32));
    let open = |controller: &mut ExtensionController, id: &str| {
        controller.on_engine_event(
            WebViewEvent::ExtensionSidePanel {
                id: id.into(),
                change: ExtensionSidePanelChange::Open(format!(
                    "chrome-extension://{id}/panel.html"
                )),
            },
            &shared,
        );
    };
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    controller.installed = vec![with_side_panel(&notes), with_side_panel(&other)];
    controller.attach_side_panel_widths(store());
    let room = 2000.0;
    // Nothing shows: nothing to widen.
    assert!(!controller.set_side_panel_width(500.0));

    // At first the theme's width; dragged, the width the person chose.
    open(&mut controller, &notes);
    assert!((controller.side_panel_extent(&theme, room) - side_panel_width(&theme)).abs() < 0.01);
    assert!(controller.set_side_panel_width(500.0));
    assert!(!controller.set_side_panel_width(500.2), "the same width");
    assert!((controller.side_panel_extent(&theme, room) - 500.0).abs() < 0.5);
    // Never past its bounds, and the page keeps a readable column.
    controller.set_side_panel_width(10_000.0);
    assert!((controller.side_panel_extent(&theme, room) - widest).abs() < 0.01);
    controller.set_side_panel_width(1.0);
    assert!((controller.side_panel_extent(&theme, room) - narrowest).abs() < 0.01);
    let reading = theme.tokens.metrics.reading_min_width;
    controller.set_side_panel_width(widest);
    let narrow_room = narrowest + reading + 40.0;
    assert!(
        (controller.side_panel_extent(&theme, narrow_room) - (narrow_room - reading)).abs() < 0.01
    );
    controller.set_side_panel_width(480.0);
    controller.remember_side_panel_width();

    // Each extension's panel has its own width: another's stands at the
    // theme's until dragged.
    open(&mut controller, &other);
    assert!((controller.side_panel_extent(&theme, room) - side_panel_width(&theme)).abs() < 0.01);

    // A later window finds the width kept on this device.
    let mut later = ExtensionController::new(true);
    later.refresh = false;
    later.installed = controller.installed.clone();
    later.attach_side_panel_widths(store());
    open(&mut later, &notes);
    assert!((later.side_panel_extent(&theme, room) - 480.0).abs() < 0.01);
    assert_eq!(store().scope(), ui::storage::Scope::DeviceLocal);
}

#[test]
fn a_shortcut_pressed_in_the_address_runs_the_command_that_holds_it() {
    use ui::webview::{ExtensionShortcut, KeyInput, KeyModifiers};
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    let alt_shift = KeyModifiers {
        alt: true,
        shift: true,
        ..KeyModifiers::default()
    };
    let key = |text: &str| KeyInput::Character(text.into());
    // Nothing held yet: the field keeps every key.
    assert!(!controller.command_key(&key("K"), alt_shift, &shared));
    controller.on_engine_event(
        WebViewEvent::ExtensionShortcuts(vec![ExtensionShortcut {
            key: "K".into(),
            modifiers: alt_shift,
        }]),
        &shared,
    );
    assert!(controller.command_key(&key("K"), alt_shift, &shared));
    assert!(
        controller.command_key(&key("k"), alt_shift, &shared),
        "as typed"
    );
    assert_eq!(
        window
            .commands()
            .iter()
            .filter(|command| **command
                == WebViewCommand::ExtensionCommandKey {
                    key: key("K"),
                    modifiers: alt_shift,
                })
            .count(),
        1
    );
    // Another key, or the same with other modifiers, stays the field's.
    assert!(!controller.command_key(&key("J"), alt_shift, &shared));
    assert!(!controller.command_key(
        &key("K"),
        KeyModifiers {
            alt: true,
            ..KeyModifiers::default()
        },
        &shared
    ));
    // Told none are held any more, it runs none.
    controller.on_engine_event(WebViewEvent::ExtensionShortcuts(Vec::new()), &shared);
    assert!(!controller.command_key(&key("K"), alt_shift, &shared));
}
