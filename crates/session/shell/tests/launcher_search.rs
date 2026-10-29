//! Search through the launcher app: the shell mounts it, opens it through
//! its action when the person asks for search, answers what it types and
//! runs what it chooses.

use shell::shell::hosted_inputs::HostedTileInputs;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::WorkspaceTree;

fn harness() -> ui::headless::FrameHarness {
    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.own_workspaces_for_test(|_| shell::shell::state::local_user_id());
    let inputs = HostedTileInputs::fixture().expect("owned hosted inputs");
    let (mut host, _chrome) = shell::shell::build_tilo_overlay_host_with_inputs(
        tree.shared(),
        false,
        ui::record::RecordStore::in_memory(),
        inputs,
    );
    host.set_shortcut_config(settings_store::hardware::ShortcutConfig::default())
        .unwrap();
    ui::headless::FrameHarness::for_root(
        ui::state::State::new(),
        host,
        (1280, 800),
        ui::theme::Theme::dark(),
    )
}

fn launcher(frame: &mut ui::headless::FrameHarness) -> (bool, Vec<String>, vello::kurbo::Rect) {
    frame.relayout();
    frame
        .root::<ShellOverlayHost>()
        .launcher_for_tests()
        .expect("the session mounts the launcher")
}

#[test]
fn super_space_opens_the_launcher_which_searches_as_the_person_types() {
    let mut frame = harness();
    frame.relayout();
    frame.set_modifiers(&["super".to_owned()]);
    frame.key("Space");
    frame.set_modifiers(&[]);
    let (open, rows, card) = launcher(&mut frame);
    assert!(frame.root::<ShellOverlayHost>().palette_visible());
    assert!(open, "the launcher opened through its own action");
    assert!(!rows.is_empty(), "it lists where to start");
    assert!(card.width() > 0.0, "and shows its card: {card:?}");

    frame.type_text("term");
    let (_, rows, _) = launcher(&mut frame);
    assert_eq!(frame.root::<ShellOverlayHost>().palette_query(), "term");
    assert!(!rows.is_empty(), "its rows answer what was typed");

    frame.key("Escape");
    frame.key("Escape");
    let (open, _, _) = launcher(&mut frame);
    assert!(!frame.root::<ShellOverlayHost>().palette_visible());
    assert!(!open, "closing search closes the launcher");
}
