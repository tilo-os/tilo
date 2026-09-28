//! Display layout and identification, end to end through the real overlay
//! host over a compositor-published display snapshot: Super+P opens the
//! layout sheet, keys choose a layout, and the staged display transaction
//! the compositor applies is read back from the settings file it watches;
//! an Identify request from Settings puts a numbered card on every display.

use std::collections::{BTreeMap, HashMap};

use settings_store::display_identify::IdentifyRequest;
use settings_store::hardware::{
    DisplayHardwareSnapshot, DisplayOutputSnapshot, DisplayTransaction, DisplayTransactionState,
    LiveOutputPlacement, OutputDescriptor, OutputMode, publisher_identity,
};
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::OutputGeometry;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::state::WorkspaceTree;
use taffy::prelude::TaffyMaxContent;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::a11y::{AccessNode, Role};
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::{Point, Rect};
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, ModifiersState, NamedKey, PhysicalKey};

fn mode(width: u32, height: u32) -> OutputMode {
    OutputMode {
        width,
        height,
        refresh_mhz: 60_000,
    }
}

fn output(connector: &str, builtin: bool, modes: &[OutputMode]) -> DisplayOutputSnapshot {
    DisplayOutputSnapshot {
        descriptor: OutputDescriptor {
            connector: connector.into(),
            connector_label: connector.into(),
            modes: modes.to_vec(),
            hdr: false,
            vrr: false,
            color_profiles: Vec::new(),
            monitor: None,
        },
        current_mode: modes[0],
        scale: 1.0,
        builtin,
        sink_hdr: false,
        hdr_unavailable_reason: None,
        sink_vrr: false,
        vrr_unavailable_reason: None,
        color_profile_labels: BTreeMap::new(),
    }
}

fn placement(connector: &str, x: i32, width: u32, height: u32) -> LiveOutputPlacement {
    LiveOutputPlacement {
        connector: connector.into(),
        x,
        y: 0,
        width,
        height,
    }
}

/// Publish a laptop panel and an HDMI monitor exactly as the compositor
/// does: the runtime snapshot, stamped with a live publisher identity.
fn publish_laptop_with_monitor() {
    let (publisher_pid, publisher_start_ticks) =
        publisher_identity().expect("hold the publisher lease, as the compositor does");
    DisplayHardwareSnapshot {
        generated_unix_ms: 1,
        backend: "drm".into(),
        publisher_pid,
        publisher_start_ticks,
        outputs: vec![
            output("eDP-1", true, &[mode(2560, 1600), mode(1920, 1080)]),
            output("HDMI-A-1", false, &[mode(3840, 2160), mode(1920, 1080)]),
        ],
        layout: vec![
            placement("eDP-1", 0, 2560, 1600),
            placement("HDMI-A-1", 2560, 3840, 2160),
        ],
    }
    .save()
    .expect("write display snapshot");
    assert!(
        DisplayHardwareSnapshot::load().is_some(),
        "the published snapshot must be trusted, or the test observes nothing"
    );
}

pub(crate) fn layout_host(host: &mut ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
    use taffy::{Dimension, Style};
    let theme = Theme::dark();
    let mut taffy: TaffyTree<()> = TaffyTree::new();
    let mut node_map: HashMap<NodeId, *mut dyn Widget> = HashMap::new();
    let host_node = host.build_layout(&mut taffy, &theme, &mut node_map);
    let root = taffy
        .new_with_children(
            Style {
                size: Size {
                    width: Dimension::length(1600.0),
                    height: Dimension::length(900.0),
                },
                ..Default::default()
            },
            &[host_node],
        )
        .unwrap();
    let map_for_measure = node_map.clone();
    let mut tr = ui::text_render::TextRenderer::new();
    taffy
        .compute_layout_with_measure(
            root,
            Size {
                width: AvailableSpace::Definite(1600.0),
                height: AvailableSpace::Definite(900.0),
            },
            |known, available, node_id, _, _| {
                if let Some(&ptr) = map_for_measure.get(&node_id) {
                    unsafe { (*ptr).measure(known, available, &mut tr) }
                } else {
                    taffy::Size::ZERO
                }
            },
        )
        .or_else(|_| taffy.compute_layout(root, taffy::Size::MAX_CONTENT))
        .expect("layout must compute");
    host.apply_layout(&taffy, host_node, Point::ZERO);
    (taffy, host_node)
}

/// Route a pressed logical key through the host dispatcher. `KeyEvent` has
/// no public constructor, so its opaque platform payload is zeroed and every
/// public field set, as the compositor's own harness does.
pub(crate) fn press(
    host: &mut ShellOverlayHost,
    layout: &(TaffyTree<()>, NodeId),
    key: Key,
    mods: ModifiersState,
) {
    let mut key_event: KeyEvent = unsafe { std::mem::zeroed() };
    key_event.logical_key = key;
    key_event.physical_key =
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    key_event.location = KeyLocation::Standard;
    key_event.state = ElementState::Pressed;
    key_event.repeat = false;
    key_event.text = None;
    let event = WindowEvent::KeyboardInput {
        device_id: unsafe { std::mem::zeroed() },
        event: key_event,
        is_synthetic: false,
    };
    let mut state = automerge::AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    host.handle_event(
        &event,
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::from(mods),
            state: &mut state,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: layout.1,
            taffy: &layout.0,
        },
    );
    assert!(handled, "{event:?} must be consumed by the shell");
}

fn super_p(host: &mut ShellOverlayHost, layout: &(TaffyTree<()>, NodeId)) {
    press(
        host,
        layout,
        Key::Character("p".into()),
        ModifiersState::SUPER,
    );
}

fn staged() -> DisplayTransaction {
    let transaction = DisplayTransaction::load().expect("a display transaction was staged");
    assert_eq!(transaction.state, DisplayTransactionState::Requested);
    transaction
}

/// The display sheet in the host's semantic tree.
fn display_sheet(host: &ShellOverlayHost) -> AccessNode {
    host.accessibility()
        .expect("the host publishes a semantic tree")
        .children
        .into_iter()
        .find(|node| node.role == Role::Dialog && node.name.as_deref() == Some("Display"))
        .expect("the open display sheet is in the semantic tree")
}

fn highlighted_layout(host: &ShellOverlayHost) -> String {
    display_sheet(host)
        .children
        .into_iter()
        .find(|node| node.state.selected)
        .and_then(|node| node.name)
        .expect("one layout is highlighted")
}

#[test]
fn super_p_arrow_enter_stages_a_mirror_transaction() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    let layout = layout_host(&mut host);

    super_p(&mut host, &layout);
    assert!(
        chrome.borrow().overlays.projection,
        "Super+P opens the sheet"
    );
    assert_eq!(
        highlighted_layout(&host),
        "Extend",
        "a fresh laptop-plus-monitor is extended, so the current layout is highlighted"
    );

    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::ArrowDown),
        ModifiersState::empty(),
    );
    assert_eq!(highlighted_layout(&host), "Mirror");
    assert!(
        DisplayTransaction::load().is_none(),
        "moving the highlight changes nothing on the displays"
    );
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );

    let transaction = staged();
    let monitor = &transaction.candidate.outputs["HDMI-A-1"];
    let panel = &transaction.candidate.outputs["eDP-1"];
    assert_eq!(monitor.mirror_of.as_deref(), Some("eDP-1"));
    assert!(panel.enabled && monitor.enabled && panel.primary);
    assert_eq!(panel.mode, Some(mode(1920, 1080)));
    assert_eq!(monitor.mode, Some(mode(1920, 1080)));

    let sheet = display_sheet(&host);
    for name in ["Keep", "Revert"] {
        assert!(
            sheet
                .children
                .iter()
                .any(|node| node.name.as_deref() == Some(name)),
            "a staged layout offers {name} until it restores itself"
        );
    }
}

#[test]
fn super_p_again_cycles_and_enter_applies_the_highlighted_layout() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    let layout = layout_host(&mut host);

    super_p(&mut host, &layout);
    let mut cycle = Vec::new();
    for _ in 0..4 {
        super_p(&mut host, &layout);
        assert!(
            chrome.borrow().overlays.projection,
            "Super+P while open cycles instead of closing"
        );
        cycle.push(highlighted_layout(&host));
    }
    assert_eq!(
        cycle,
        ["Mirror", "Only this screen", "Only external", "Extend"]
    );
    assert!(
        DisplayTransaction::load().is_none(),
        "cycling alone stages nothing"
    );

    super_p(&mut host, &layout);
    super_p(&mut host, &layout);
    assert_eq!(highlighted_layout(&host), "Only this screen");
    press(
        &mut host,
        &layout,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    );

    let transaction = staged();
    assert!(transaction.candidate.outputs["eDP-1"].enabled);
    assert!(!transaction.candidate.outputs["HDMI-A-1"].enabled);
}

#[test]
fn identify_puts_a_numbered_card_on_every_display() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    publish_laptop_with_monitor();
    let (mut host, _chrome) = build_tilo_overlay_host_with_chrome(&WorkspaceTree::bootstrap_demo());
    let outputs = [
        OutputGeometry::unscaled(0.0, 0.0, 2560.0, 1600.0),
        OutputGeometry::unscaled(2560.0, 0.0, 3840.0, 2160.0),
    ];
    host.set_output_geometry(&outputs);
    let _layout = layout_host(&mut host);
    assert!(host.display_identify_labels().is_empty());
    assert!(!host.poll_display_identify(), "nothing was asked yet");

    IdentifyRequest::request().expect("Settings writes the request");
    assert!(host.poll_display_identify());
    let labels = host.display_identify_labels();
    assert_eq!(labels.len(), outputs.len(), "one card per display");
    for (label, output) in labels.iter().zip(&outputs) {
        assert_eq!(
            label.output,
            Rect::new(output.x, 0.0, output.x + output.width, output.height),
            "each card is centred on its own display"
        );
    }
    let named: Vec<(usize, &str)> = labels
        .iter()
        .map(|label| (label.number, label.name.as_str()))
        .collect();
    assert_eq!(named, [(1, "Built-in display"), (2, "HDMI-A-1")]);
    assert!(
        !host.poll_display_identify(),
        "one request shows the numbers once"
    );
}
