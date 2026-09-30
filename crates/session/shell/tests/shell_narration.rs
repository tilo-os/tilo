//! The shell narrates its own chrome: with a recording speaker behind the
//! toolkit screen reader, moving focus in Quick Settings speaks the focused
//! control, a notification's arrival is spoken politely (an error cuts
//! in), and a hardware-key readout is announced.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use settings_store::hardware::QuickSettingsConfig;
use shell::shell::build_tilo_overlay_host_with_chrome;
use shell::shell::overlay_host::ShellOverlayHost;
use shell::shell::shell_narrator::ShellNarrator;
use shell::shell::state::{FreshId as _, WorkspaceId, WorkspaceTree};
use taffy::prelude::TaffyMaxContent;
use taffy::{AvailableSpace, NodeId, Size, TaffyTree};
use ui::screen_reader::ScreenReader;
use ui::theme::Theme;
use ui::widget::Widget;
use vello::kurbo::Point;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::keyboard::{Key, KeyLocation, NamedKey, PhysicalKey};

#[derive(Clone, Default)]
struct RecordingSpeaker(Arc<Mutex<Vec<tts::Utterance>>>);

impl RecordingSpeaker {
    fn take(&self) -> Vec<tts::Utterance> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}

impl tts::Speaker for RecordingSpeaker {
    fn speak_utterance(&mut self, utterance: tts::Utterance) {
        self.0.lock().unwrap().push(utterance);
    }
    fn stop(&mut self) {}
    fn set_rate(&mut self, _rate: f32) {}
    fn set_voice(&mut self, _voice: &str) {}
    fn is_speaking(&self) -> bool {
        false
    }
    fn backend_name(&self) -> &'static str {
        "recording"
    }
}

fn tree() -> WorkspaceTree {
    let ws = shell::shell::state::Workspace::new(WorkspaceId::new(), "W");
    let active = ws.id;
    WorkspaceTree {
        desktop: shell::shell::state::DesktopTree {
            workspaces: vec![ws],
            active,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn layout_host(host: &mut ShellOverlayHost) -> (TaffyTree<()>, NodeId) {
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

fn press(host: &mut ShellOverlayHost, taffy: &TaffyTree<()>, node: NodeId, key: NamedKey) {
    let mut key_event: KeyEvent = unsafe { std::mem::zeroed() };
    key_event.logical_key = Key::Named(key);
    key_event.physical_key =
        PhysicalKey::Unidentified(winit::keyboard::NativeKeyCode::Unidentified);
    key_event.location = KeyLocation::Standard;
    key_event.state = ElementState::Pressed;
    key_event.repeat = false;
    key_event.text = None;
    let mut state = automerge::AutoCommit::new();
    let mut handled = false;
    let mut focus_target = None;
    host.handle_event(
        &WindowEvent::KeyboardInput {
            device_id: unsafe { std::mem::zeroed() },
            event: key_event,
            is_synthetic: false,
        },
        &mut ui::widget::EventCtx {
            scale_factor: 1.0,
            modifiers: &Modifiers::default(),
            state: &mut state,
            path: &automerge::ROOT,
            handled: &mut handled,
            focus_target: &mut focus_target,
            node_id: node,
            taffy,
        },
    );
}

#[test]
fn tab_in_quick_settings_speaks_the_focused_control() {
    let (host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    let mut host = host.with_quick_settings_config(QuickSettingsConfig::default());
    let speaker = RecordingSpeaker::default();
    let mut narrator =
        ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(speaker.clone())));
    {
        let mut state = chrome.borrow_mut();
        state.overlays.quick_settings = true;
        state.quick_settings.brightness_available = true;
        state.bump();
    }
    host.force_quick_settings_settled();
    let (taffy, node) = layout_host(&mut host);
    narrator.observe(&host.chrome_narration_snapshot());
    assert!(
        speaker.take().is_empty(),
        "opening with nothing focused says nothing yet"
    );

    press(&mut host, &taffy, node, NamedKey::Tab);
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    let first = spoken.first().expect("Tab speaks the control it lands on");
    assert!(
        first.text.starts_with("Brightness, slider, "),
        "got {:?}",
        first.text
    );
    assert!(first.interrupt, "a focus move interrupts older speech");

    narrator.observe(&host.chrome_narration_snapshot());
    assert!(speaker.take().is_empty(), "an unchanged frame is silent");

    press(&mut host, &taffy, node, NamedKey::Tab);
    narrator.observe(&host.chrome_narration_snapshot());
    drop(host);
    let spoken = speaker.take();
    assert!(
        spoken
            .first()
            .is_some_and(|u| u.text.starts_with("Volume, slider, ")),
        "got {spoken:?}"
    );
}

#[test]
fn a_notification_arrival_is_spoken_politely_and_an_error_cuts_in() {
    let (host, chrome) = build_tilo_overlay_host_with_chrome(&tree());
    let speaker = RecordingSpeaker::default();
    let mut narrator =
        ShellNarrator::with_reader(ScreenReader::with_speaker(Box::new(speaker.clone())));
    narrator.observe(&host.chrome_narration_snapshot());
    assert!(speaker.take().is_empty());

    chrome.borrow_mut().push_notification("New mail", "Lunch?");
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert_eq!(spoken.len(), 1, "got {spoken:?}");
    assert_eq!(spoken[0].text, "New mail. Lunch?");
    assert_eq!(spoken[0].priority, tts::Priority::Notification);
    assert!(!spoken[0].interrupt, "a polite arrival never interrupts");

    chrome
        .borrow_mut()
        .push_error_notification("Backup failed", "The drive is not connected");
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert_eq!(
        spoken
            .first()
            .map(|u| (u.text.as_str(), u.priority, u.interrupt)),
        Some((
            "Backup failed. The drive is not connected",
            tts::Priority::Important,
            true
        ))
    );

    chrome
        .borrow_mut()
        .show_osd(shell::shell::osd::Osd::volume(0.5, false));
    narrator.observe(&host.chrome_narration_snapshot());
    let spoken = speaker.take();
    assert!(
        spoken.iter().any(|u| u.text == "Volume 50%"),
        "a hardware-key readout is announced: {spoken:?}"
    );
}
