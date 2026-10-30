//! Every surface the shell hosts as an app (the top bar, the launcher,
//! Quick Settings, the notification banners, the overview and the sheets)
//! is driven through the controls it names, as assistive technology, a test
//! or an agent drives it: one name per control, no label and no place on
//! screen.

use settings_store::hardware::QuickSettingsConfig;
use shell::shell::chrome_state::Chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::WorkspaceTree;
use ui::a11y::Action;
use ui::headless::FrameHarness;

/// The demo desktop with its chrome, drawn once.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the host moves into the harness at its last use; nothing is held past it"
)]
fn harness() -> (FrameHarness, Chrome) {
    let tree = WorkspaceTree::bootstrap_demo();
    let (host, chrome) = shell::shell::build_tilo_overlay_host_with_chrome(&tree);
    let mut host = host.with_quick_settings_config(QuickSettingsConfig::default());
    host.disable_quick_settings_os_reads();
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .unwrap();
    let mut frame = FrameHarness::for_root(
        ui::state::State::new(),
        host,
        (1440, 900),
        ui::theme::Theme::dark(),
    );
    frame.relayout();
    (frame, chrome)
}

/// A few frames, so what an action asked is carried out and shown.
fn settle(frame: &mut FrameHarness) {
    for _ in 0..3 {
        frame.relayout();
    }
}

#[test]
fn the_top_bar_is_driven_by_its_control_names() {
    let (mut frame, _chrome) = harness();
    frame
        .perform_semantic_named("topbar:workspace-settings", None, &Action::Activate)
        .unwrap();
    settle(&mut frame);
    assert!(
        frame
            .root::<ShellOverlayHost>()
            .workspace_menu_labels()
            .is_some(),
        "the bar's workspace control opens the workspace menu"
    );
}

#[test]
fn the_launcher_is_driven_by_its_control_names() {
    let (mut frame, _chrome) = harness();
    frame.set_modifiers(&["super".to_owned()]);
    frame.key("Space");
    frame.set_modifiers(&[]);
    settle(&mut frame);
    assert!(frame.root::<ShellOverlayHost>().palette_visible());
    // The launcher's field takes a query set by its name, as typing it
    // would.
    frame
        .perform_semantic_named(
            "command-palette:query",
            None,
            &Action::SetValue("term".into()),
        )
        .unwrap();
    settle(&mut frame);
    assert_eq!(frame.root::<ShellOverlayHost>().palette_query(), "term");
}

#[test]
fn quick_settings_is_driven_by_its_control_names() {
    let (mut frame, chrome) = harness();
    chrome.borrow_mut().toggle_quick_settings();
    frame
        .root_mut::<ShellOverlayHost>()
        .force_quick_settings_settled();
    settle(&mut frame);
    assert!(
        frame
            .root::<ShellOverlayHost>()
            .quick_settings_target("qs-edit")
            .is_some(),
        "the panel shows its Edit button"
    );
    frame
        .perform_semantic_named("qs-edit", None, &Action::Activate)
        .unwrap();
    frame
        .root_mut::<ShellOverlayHost>()
        .force_quick_settings_settled();
    settle(&mut frame);
    assert!(
        frame
            .root::<ShellOverlayHost>()
            .quick_settings_target("qs-edit-done")
            .is_some(),
        "Edit, pressed by its name, arranges the panel's controls"
    );
}

#[test]
fn a_notification_banner_is_driven_by_its_control_names() {
    let (mut frame, chrome) = harness();
    let id = chrome.borrow_mut().push_notification("New mail", "Lunch?");
    settle(&mut frame);
    frame
        .perform_semantic_named(
            &format!("notification:{id}:dismiss"),
            None,
            &Action::Activate,
        )
        .unwrap();
    settle(&mut frame);
    assert!(
        !chrome
            .borrow()
            .notifications()
            .iter()
            .any(|notification| notification.id == id),
        "the banner's dismiss control, pressed by its name, dismisses it"
    );
}

#[test]
fn the_overview_is_driven_by_its_control_names() {
    let (mut frame, chrome) = harness();
    let (active, other) = {
        let state = chrome.borrow();
        let desktop = &state.tree().desktop;
        let other = desktop
            .workspaces
            .iter()
            .map(|workspace| workspace.id)
            .find(|id| *id != desktop.active)
            .expect("the demo desktop has a second workspace");
        (desktop.active, other)
    };
    {
        let mut state = chrome.borrow_mut();
        state.overlays.overview = true;
        state.bump();
    }
    frame
        .root_mut::<ShellOverlayHost>()
        .force_overview_settled(true);
    settle(&mut frame);
    assert!(frame.root::<ShellOverlayHost>().overview_open());
    frame
        .perform_semantic_named(
            &format!("overview-room:{}", other.raw()),
            None,
            &Action::Activate,
        )
        .unwrap();
    settle(&mut frame);
    let now = chrome.borrow().tree().desktop.active;
    assert_ne!(now, active, "the room, chosen by its name, is gone to");
    assert_eq!(now, other);
}

#[test]
fn a_sheet_is_driven_by_its_control_names() {
    let (mut frame, chrome) = harness();
    frame
        .root_mut::<ShellOverlayHost>()
        .force_power_menu_for_visual_test();
    settle(&mut frame);
    assert!(frame.root::<ShellOverlayHost>().power_menu_open());
    let notices = chrome.borrow().notifications().len();
    frame
        .perform_semantic_named("power_lock", None, &Action::Activate)
        .unwrap();
    settle(&mut frame);
    // The row did what it does: the session is asked to lock, or a notice
    // says why it may not here.
    let locked = chrome.borrow_mut().take_session_lock_request();
    let refused = chrome.borrow().notifications().len() > notices;
    assert!(
        locked || refused,
        "Lock, pressed by its name, asks for the lock"
    );
}
