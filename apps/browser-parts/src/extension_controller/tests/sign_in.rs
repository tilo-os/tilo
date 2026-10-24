//! An extension signing in: its page shown over the window, or loaded out of
//! sight.

use super::*;

#[test]
fn an_extension_signs_in_on_a_page_shown_over_the_window() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    controller.installed = vec![installed("signer", None, toolbar::Access::default())];
    let page = "https://accounts.example/authorize?client_id=1";
    let sign_in = |id: &str| WebViewEvent::ExtensionSignIn {
        id: id.into(),
        url: page.into(),
        interactive: true,
        flow: 1,
    };
    // Only an extension that runs.
    assert!(!controller.on_engine_event(sign_in("stranger"), &shared));
    assert!(window.created_tabs().is_empty());

    assert!(controller.on_engine_event(sign_in("signer"), &shared));
    let created = window.created_tabs();
    assert_eq!(created.len(), 1, "the sign-in page's tab");
    let commands = created[0].commands();
    let marked = commands.iter().position(|c| {
        *c == WebViewCommand::ExtensionSignInPanel {
            id: "signer".into(),
            flow: 1,
        }
    });
    let navigate = commands
        .iter()
        .position(|c| matches!(c, WebViewCommand::Navigate(url) if url == page));
    assert!(
        marked.is_some() && marked < navigate,
        "marked as the extension's sign-in before its page loads: {commands:?}"
    );
    assert!(
        !commands
            .iter()
            .any(|c| matches!(c, WebViewCommand::ExtensionPanel { .. })),
        "no popup: the extension gets no page's access from it"
    );
    let site = |controller: &ExtensionController| match &controller.panel {
        Some(Panel::Popup { site, .. }) => site.clone(),
        _ => None,
    };
    // It is as large as a panel gets, and names the site it is on.
    assert_eq!(controller.page_width(2000.0), Some(POPUP_MAX.0));
    assert_eq!(site(&controller).as_deref(), Some("accounts.example"));
    created[0].push_event(WebViewEvent::UriChanged(
        "https://login.example/password".into(),
    ));
    assert!(controller.sync(&shared, ""));
    assert_eq!(site(&controller).as_deref(), Some("login.example"));
    // The sign-in ended: the engine closed its page, and the panel goes.
    created[0].push_event(WebViewEvent::TabClosed);
    assert!(controller.sync(&shared, ""));
    assert!(controller.panel.is_none());

    // One that may not ask the person loads out of sight: no panel.
    let quiet = |flow: u32| WebViewEvent::ExtensionSignIn {
        id: "signer".into(),
        url: page.into(),
        interactive: false,
        flow,
    };
    assert!(!controller.on_engine_event(quiet(2), &shared));
    assert!(controller.panel.is_none());
    let created = window.created_tabs();
    assert_eq!(created.len(), 2);
    assert_eq!(
        created[1].commands()[..2],
        [
            WebViewCommand::ExtensionSignInPanel {
                id: "signer".into(),
                flow: 2,
            },
            WebViewCommand::Navigate(page.into()),
        ]
    );
    // A few at most; the engine closes each as its sign-in ends.
    for flow in 3..8 {
        controller.on_engine_event(quiet(flow), &shared);
    }
    assert_eq!(controller.quiet_sign_ins.len(), QUIET_SIGN_INS);
    window.created_tabs()[6].push_event(WebViewEvent::TabClosed);
    controller.sync(&shared, "");
    assert_eq!(controller.quiet_sign_ins.len(), QUIET_SIGN_INS - 1);
}

#[test]
fn the_page_that_asked_for_a_sign_in_waits_beneath_it_and_shows_again() {
    let window = ui::webview::test_engine::TestEngine::new();
    let shared: SharedEngine = window.clone();
    let mut controller = ExtensionController::new(true);
    controller.refresh = false;
    controller.installed = vec![
        installed("signer", None, toolbar::Access::default()),
        installed("other", None, toolbar::Access::default()),
    ];
    let sign_in = |id: &str, flow: u32| WebViewEvent::ExtensionSignIn {
        id: id.into(),
        url: "https://accounts.example/authorize".into(),
        interactive: true,
        flow,
    };
    let settings = ui::webview::test_engine::TestEngine::new();
    let show_settings = |controller: &mut ExtensionController| {
        controller.panel = Some(Panel::Popup {
            id: "settings:signer".into(),
            engine: settings.clone(),
            size: Some(POPUP_MAX),
            site: None,
            beneath: None,
        });
    };

    // Its settings page asks the person to sign in: the sign-in page shows,
    // and the settings page, whose call waits to hear how it ends, goes on
    // beneath it rather than closing.
    show_settings(&mut controller);
    assert!(controller.on_engine_event(sign_in("signer", 1), &shared));
    let first: SharedEngine = window.created_tabs()[0].clone();
    assert!(shows(&controller, &first));
    // A second sign-in of the extension shows over the same page.
    assert!(controller.on_engine_event(sign_in("signer", 2), &shared));
    let second: SharedEngine = window.created_tabs()[1].clone();
    assert!(shows(&controller, &second));
    // What it asks for meanwhile is still the window's.
    settings.push_new_tab_request("https://signer.example/welcome");
    controller.sync(&shared, "");
    let beneath: SharedEngine = settings.clone();
    assert!(
        beneath.take_new_tab_requests().is_empty(),
        "drained, not piling up"
    );
    assert!(shows(&controller, &second));
    // The sign-in ended and its page closed: the settings page shows again.
    window.created_tabs()[1].push_event(WebViewEvent::TabClosed);
    assert!(controller.sync(&shared, ""));
    assert!(shows(&controller, &beneath), "the settings page that asked");

    // A page that closed itself while the person signed in stays closed.
    assert!(controller.on_engine_event(sign_in("signer", 3), &shared));
    settings.push_event(WebViewEvent::TabClosed);
    controller.sync(&shared, "");
    window.created_tabs()[2].push_event(WebViewEvent::TabClosed);
    assert!(controller.sync(&shared, ""));
    assert!(controller.panel.is_none());

    // Dismissing the sign-in closes the page beneath with it, and another
    // extension's page is not kept under a sign-in.
    show_settings(&mut controller);
    assert!(controller.on_engine_event(sign_in("signer", 4), &shared));
    controller.dismiss();
    assert!(controller.panel.is_none());
    show_settings(&mut controller);
    assert!(controller.on_engine_event(sign_in("other", 5), &shared));
    window.created_tabs()[4].push_event(WebViewEvent::TabClosed);
    assert!(controller.sync(&shared, ""));
    assert!(controller.panel.is_none());
}

/// Whether the panel shows `page`.
fn shows(controller: &ExtensionController, page: &SharedEngine) -> bool {
    match &controller.panel {
        Some(Panel::Popup { engine, .. }) => Arc::ptr_eq(engine, page),
        _ => false,
    }
}
